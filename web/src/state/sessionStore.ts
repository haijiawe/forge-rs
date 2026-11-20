// Per-session store: the stream machine, the row projection and the frame
// batching that keeps a token stream at one React render per animation frame.
//
// The machine is applied synchronously as frames arrive (ordering must not wait
// for a frame), but the render snapshot is only rebuilt and published from a
// requestAnimationFrame callback. A burst of 30 delta frames in one frame
// therefore produces one render, which is the requirement in
// docs/05-roadmap.md M3 ("no jank while streaming").

import { ApiError } from '../api/client';
import type { HearthApi, OutgoingAttachment } from '../api/client';
import type { Event } from '../api/events';
import { latestToolNames } from './capabilities';
import { StreamMachine, type StreamInput, type StreamState } from '../api/stream';
import { SessionStream, type StreamEvent } from '../api/transport';
import type { ApprovalDecision, PendingApproval, ServerFrame, SessionDetail } from '../api/types';
import { RowAccumulator, compressedBlocks, type CompressedBlock, type Row } from './rows';

export interface SessionViewState {
  /** Monotonic; useSyncExternalStore subscribers diff on this. */
  version: number;
  session: SessionDetail | null;
  status: string;
  lastSeq: number;
  headSeq: number;
  rows: Row[];
  rowKeys: string[];
  /**
   * `context/compressed` blocks the log has written, sorted by `from`. The rows
   * inside a range are marked `foldedBy` in `rows`; this is the list to count
   * them by, for the header.
   */
  blocks: CompressedBlock[];
  /** Tool names from the newest `llm/call`; empty before the first request. */
  toolNames: string[];
  pendingApproval: PendingApproval | null;
  streamError: string | null;
  connecting: boolean;
  needsResync: boolean;
  /** User-facing error from a failed mutation (send/decide). */
  actionError: string | null;
}

export interface SessionStoreOptions {
  api: HearthApi;
  sessionId: string;
  token: string;
  baseUrl?: string;
  webSocketImpl?: typeof WebSocket;
  /** Injected in tests so flushes are deterministic. */
  scheduleFrame?: (callback: () => void) => number;
  cancelFrame?: (handle: number) => void;
  /** Initial page size for the REST transcript load and resyncs. */
  pageSize?: number;
}

function defaultScheduleFrame(callback: () => void): number {
  if (typeof requestAnimationFrame === 'function') return requestAnimationFrame(callback);
  return setTimeout(callback, 16) as unknown as number;
}

function defaultCancelFrame(handle: number): void {
  if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(handle);
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return `${error.code}: ${error.message}`;
  if (error instanceof Error) return error.message;
  return String(error);
}

export class SessionStore {
  private readonly options: SessionStoreOptions;
  private readonly machine: StreamMachine;
  private readonly accumulator = new RowAccumulator();
  private readonly listeners = new Set<() => void>();
  private stream: SessionStream | null = null;
  private state: SessionViewState;
  private version = 0;
  private frame: number | null = null;
  private projectedSeq = 0;
  private started = false;
  private resyncing = false;
  private disposing = false;

  constructor(options: SessionStoreOptions) {
    this.options = options;
    this.machine = new StreamMachine(options.sessionId, 1);
    this.state = {
      version: 0,
      session: null,
      status: 'idle',
      lastSeq: 0,
      headSeq: 0,
      rows: [],
      rowKeys: [],
      blocks: [],
      toolNames: [],
      pendingApproval: null,
      streamError: null,
      connecting: false,
      needsResync: false,
      actionError: null,
    };
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): SessionViewState => this.state;

  /** Load the transcript over REST, then follow the stream from lastSeq. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.state = { ...this.state, connecting: true };
    void this.loadInitial();
  }

  stop(): void {
    this.disposing = true;
    this.stream?.close();
    this.stream = null;
    if (this.frame !== null) {
      (this.options.cancelFrame ?? defaultCancelFrame)(this.frame);
      this.frame = null;
    }
  }

  private async loadInitial(): Promise<void> {
    const { api, sessionId } = this.options;
    try {
      const session = await api.getSession(sessionId);
      if (this.disposing) return;
      this.machine.state.session = session;
      // The first page is a resync page: the machine requires it to start at
      // seq 1 and to be dense, which GET /events guarantees.
      await this.pullPages();
    } catch (error) {
      if (this.disposing) return;
      this.setActionError(errorMessage(error));
    }
    if (this.disposing) return;
    this.openStream();
    this.schedule();
  }

  private openStream(): void {
    this.stream = new SessionStream({
      sessionId: this.options.sessionId,
      token: this.options.token,
      after: () => this.machine.state.lastSeq,
      ...(this.options.baseUrl === undefined ? {} : { baseUrl: this.options.baseUrl }),
      ...(this.options.webSocketImpl === undefined
        ? {}
        : { webSocketImpl: this.options.webSocketImpl }),
      onEvent: (event) => this.onStreamEvent(event),
    });
    this.stream.start();
  }

  private onStreamEvent(event: StreamEvent): void {
    if (this.disposing) return;
    switch (event.type) {
      case 'open':
        this.state = { ...this.state, connecting: false, streamError: null };
        this.schedule();
        return;
      case 'close':
        this.state = { ...this.state, connecting: true };
        this.schedule();
        return;
      case 'parse-error':
        this.state = { ...this.state, streamError: event.message };
        this.schedule();
        return;
      case 'frame':
        this.applyFrame(event.frame);
        return;
    }
  }

  private applyFrame(frame: ServerFrame): void {
    switch (frame.kind) {
      case 'snapshot':
        this.enqueue({ kind: 'snapshot', frame });
        return;
      case 'event':
        this.enqueue({ kind: 'event', frame });
        return;
      case 'delta':
        this.enqueue({ kind: 'delta', frame });
        return;
      case 'error':
        this.enqueue({ kind: 'stream-error', frame });
        return;
      case 'pong':
        return;
    }
  }

  private enqueue(input: StreamInput): void {
    const effects = this.machine.apply(input);
    this.handleEffects(effects);
    this.schedule();
  }

  private handleEffects(effects: ReturnType<StreamMachine['apply']>): void {
    for (const effect of effects) {
      if (effect.kind === 'resync') {
        void this.pullPages();
      } else {
        this.state = { ...this.state, streamError: effect.message, connecting: false };
      }
    }
  }

  /**
   * Fetch GET /events from the machine's cursor until it is caught up. Runs one
   * page at a time: each page is applied synchronously, so a slow server can
   * never interleave pages out of order.
   */
  private async pullPages(): Promise<void> {
    if (this.resyncing || this.disposing) return;
    this.resyncing = true;
    try {
      const { api, sessionId, pageSize = 500 } = this.options;
      for (;;) {
        const state: StreamState = this.machine.state;
        if (!state.needsResync && state.lastSeq >= state.headSeq && state.lastSeq > 0) break;
        const page = await api.events(sessionId, state.lastSeq, pageSize);
        if (this.disposing) return;
        const before = this.machine.state.lastSeq;
        this.enqueue({ kind: 'page', page });
        if (this.machine.state.lastSeq <= before) break; // no progress: stop
      }
    } catch (error) {
      if (!this.disposing) this.setActionError(errorMessage(error));
    } finally {
      this.resyncing = false;
      this.schedule();
    }
  }

  private schedule(): void {
    if (this.disposing || this.frame !== null) return;
    const scheduleFrame = this.options.scheduleFrame ?? defaultScheduleFrame;
    this.frame = scheduleFrame(() => {
      this.frame = null;
      this.flush();
    });
  }

  /** Publish one new snapshot: per-frame, so N frames cost one render. */
  private flush(): void {
    if (this.disposing) return;
    const machineState = this.machine.state;
    const events = machineState.events;
    // Only walk the events the accumulator has not seen yet.
    for (let i = this.projectedSeq; i < events.length; i += 1) {
      const event = events[i];
      if (event !== undefined) this.accumulator.push(event);
    }
    this.projectedSeq = events.length;
    // Blocks are rebuilt per flush, like toolNames: the fold marks have to
    // describe the log as it is now. A block whose `to` runs past `lastSeq` is
    // not an error — it covers seqs this window has not been sent yet — so the
    // range is kept as written and rows that arrive inside it later get marked
    // then. `lastSeq` is passed so a range that is still open here is not
    // mistaken for a range that ended before the newest event.
    const blocks = compressedBlocks(events, machineState.lastSeq);
    this.accumulator.setBlocks(blocks);
    this.accumulator.setDeltas(machineState.deltas);

    this.version += 1;
    this.state = {
      version: this.version,
      session: machineState.session,
      status: machineState.status,
      lastSeq: machineState.lastSeq,
      headSeq: machineState.headSeq,
      rows: this.accumulator.rows,
      rowKeys: this.accumulator.rowKeys(),
      blocks,
      // Computed per publish rather than stored: it is a read of the last event,
      // and a stale copy of a list that can change between turns is worse than
      // walking a few thousand events once per animation frame.
      toolNames: latestToolNames(events),
      pendingApproval: machineState.session?.pendingApproval ?? null,
      streamError: machineState.streamError,
      connecting: this.state.connecting,
      needsResync: machineState.needsResync,
      actionError: this.state.actionError,
    };
    for (const listener of this.listeners) listener();
  }

  private setActionError(message: string): void {
    this.state = { ...this.state, actionError: message };
    this.schedule();
  }

  clearActionError(): void {
    if (this.state.actionError === null) return;
    this.state = { ...this.state, actionError: null };
    this.schedule();
  }

  /** POST a user message. The transcript updates when the event arrives. */
  async sendMessage(
    text: string,
    attachments: readonly OutgoingAttachment[] = [],
  ): Promise<boolean> {
    const trimmed = text.trim();
    if (trimmed === '') return false;
    try {
      await this.options.api.postMessage(this.options.sessionId, trimmed, attachments);
      return true;
    } catch (error) {
      this.setActionError(errorMessage(error));
      return false;
    }
  }

  /** POST a message into the running turn (§4.10). The event arrives on the stream. */
  async steerTurn(text: string): Promise<boolean> {
    const trimmed = text.trim();
    if (trimmed === '') return false;
    try {
      await this.options.api.steer(this.options.sessionId, trimmed);
      return true;
    } catch (error) {
      this.setActionError(errorMessage(error));
      return false;
    }
  }

  /**
   * Ask the running turn to stop (§4.9). Nothing is written here: the loop
   * appends `turn/end`, and the stream turns the status back to idle.
   */
  async cancelTurn(): Promise<boolean> {
    try {
      await this.options.api.cancel(this.options.sessionId);
      return true;
    } catch (error) {
      this.setActionError(errorMessage(error));
      return false;
    }
  }

  /** POST an approval decision. The event clears the pending approval. */
  async decide(decision: ApprovalDecision, note?: string): Promise<boolean> {
    const pending = this.state.pendingApproval;
    if (pending === null) return false;
    try {
      await this.options.api.decide(
        this.options.sessionId,
        pending.approvalId,
        decision,
        note,
      );
      return true;
    } catch (error) {
      this.setActionError(errorMessage(error));
      return false;
    }
  }

  /** Events seen so far, for tests and diagnostics. */
  eventList(): readonly Event[] {
    return this.machine.state.events;
  }
}