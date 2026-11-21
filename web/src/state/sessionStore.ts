// Per-session store: the stream machine, the row projection and the frame
// batching that keeps a token stream at one React render per animation frame.
//
// The machine is applied synchronously as frames arrive (ordering must not wait
// for a frame), but the render snapshot is only rebuilt and published from a
// requestAnimationFrame callback. A burst of 30 delta frames in one frame
// therefore produces one render, which is the requirement in
// docs/05-roadmap.md M3 ("no jank while streaming").

import { ApiError, isTransientFailure } from '../api/client';
import type { HearthApi, OutgoingAttachment } from '../api/client';
import type { Event } from '../api/events';
import { latestToolNames } from './capabilities';
import { latestWrite, turnChangesFrom, type TurnChangeTurn } from './turnChanges';
import { StreamMachine, type StreamInput, type StreamState } from '../api/stream';
import { SessionStream, type StreamEvent } from '../api/transport';
import type { ApprovalDecision, PendingApproval, ServerFrame, SessionDetail } from '../api/types';
import {
  RowAccumulator,
  blocksFromScan,
  collectBlocks,
  emptyBlockScan,
  type CompressedBlock,
  type Row,
} from './rows';

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
  /**
   * What each turn wrote, folded from the log, oldest turn first.
   *
   * It is a read of the events rather than stored state: a turn that is still
   * running grows its rows as the writes land, and a snapshot taken at turn end
   * would be missing everything the operator watched happen.
   */
  turnChanges: TurnChangeTurn[];
  /**
   * The newest write in the log, as a pair of a sequence number and a path.
   *
   * The editor watches this to notice a file being rewritten while it is open.
   * It carries the seq so that two writes to the same file are two events: the
   * page must reload on the second one too, even though the path has not moved.
   */
  agentWrite: { seq: number; path: string } | null;
  pendingApproval: PendingApproval | null;
  streamError: string | null;
  connecting: boolean;
  needsResync: boolean;
  /** User-facing error from a failed mutation (send/decide). */
  actionError: string | null;
  /**
   * The screen could not read the session at all — the first GET, the first
   * page of events, or a resync after the stream came back with a gap.
   *
   * Separate from `actionError` because it is a different sentence: nothing
   * arrived, so the transcript below is not the truth, and the reader needs a
   * way to ask again rather than a knob that only closes the notice.
   */
  loadError: string | null;
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
  /**
   * Retry schedule for the reads that build the screen. Transient failures
   * (nothing reached the server, 5xx, 429) are tried again this many times,
   * waiting the given milliseconds before each attempt; a terminal one (401,
   * 404) is shown at once, because it would fail the same way again. Injected
   * in tests so the schedule is a list of zero waits.
   */
  retry?: { delaysMs?: readonly number[]; wait?: (ms: number) => Promise<void> };
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
  /** How far the compressed-block walk has got; see `collectBlocks`. */
  private blockScan = emptyBlockScan();
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
      turnChanges: [],
      agentWrite: null,
      pendingApproval: null,
      streamError: null,
      connecting: false,
      needsResync: false,
      actionError: null,
      loadError: null,
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

  /**
   * Run one read, and try it again while it fails in a way that can heal.
   *
   * The retry lives here rather than in each caller because every read the
   * screen needs on the way up (the session, a page of events) is safe to
   * repeat: they are GETs of an immutable prefix. The cursor moves with the
   * caller's state, so a retried page is the page the caller still needs.
   */
  private async attempt<T>(read: () => Promise<T>): Promise<T> {
    const delays = this.options.retry?.delaysMs ?? [400, 1200, 3000];
    const wait =
      this.options.retry?.wait ??
      ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
    for (let i = 0; ; i += 1) {
      try {
        return await read();
      } catch (error) {
        if (this.disposing || i >= delays.length || !isTransientFailure(error)) throw error;
        await wait(delays[i] ?? 0);
        if (this.disposing) throw error;
      }
    }
  }

  private async loadInitial(): Promise<void> {
    const { api, sessionId } = this.options;
    try {
      const session = await this.attempt(() => api.getSession(sessionId));
      if (this.disposing) return;
      this.machine.state.session = session;
      // The first page is a resync page: the machine requires it to start at
      // seq 1 and to be dense, which GET /events guarantees.
      await this.pullPages();
      if (this.state.loadError !== null) {
        this.state = { ...this.state, loadError: null };
        this.schedule();
      }
    } catch (error) {
      if (this.disposing) return;
      this.setLoadError(errorMessage(error));
    }
    if (this.disposing) return;
    // A reload after a failure must not leave a second socket behind: the one
    // opened by the first attempt is still live and still carrying the cursor.
    if (this.stream === null) this.openStream();
    this.schedule();
  }

  /**
   * Ask again after the screen failed to read the session.
   *
   * The retry schedule already tried the transient cases on its own; this is
   * the path for the reader who watched a terminal one (or an exhausted
   * schedule) and wants one more attempt before giving up on the page.
   */
  reload(): void {
    if (this.disposing || this.resyncing) return;
    // `connecting` only when there is no socket yet: with a live one the page is
    // not connecting, it is re-reading over REST, and a stuck 「连接中」 would be
    // a lie the status bar keeps telling.
    this.state = { ...this.state, loadError: null, connecting: this.stream === null };
    void this.loadInitial();
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
        // A resync that cannot read is the same situation as a first read that
        // cannot read: the transcript on screen is not the log, so it is said
        // as a load failure with a way to ask again -- not as a mutation error
        // that the reader would read as "my last action failed".
        void this.pullPages().catch((error: unknown) => {
          if (!this.disposing) this.setLoadError(errorMessage(error));
        });
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
        const page = await this.attempt(() => api.events(sessionId, state.lastSeq, pageSize));
        if (this.disposing) return;
        const before = this.machine.state.lastSeq;
        this.enqueue({ kind: 'page', page });
        if (this.machine.state.lastSeq <= before) break; // no progress: stop
      }
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
    // Blocks are collected incrementally: the walk remembers how far it got, so
    // a flush costs the events that arrived since the last one, not the whole
    // log. `blocksFromScan` re-judges `toBeyondHead` against the scan's own head,
    // because a range that ran past the reader stops doing so as events arrive.
    this.blockScan = collectBlocks(this.blockScan, events, machineState.lastSeq);
    const blocks = blocksFromScan(this.blockScan);
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
      turnChanges: turnChangesFrom(events),
      agentWrite: latestWrite(events),
      pendingApproval: machineState.session?.pendingApproval ?? null,
      streamError: machineState.streamError,
      connecting: this.state.connecting,
      needsResync: machineState.needsResync,
      actionError: this.state.actionError,
      loadError: this.state.loadError,
    };
    for (const listener of this.listeners) listener();
  }

  private setActionError(message: string): void {
    this.state = { ...this.state, actionError: message };
    this.schedule();
  }

  private setLoadError(message: string): void {
    this.state = { ...this.state, loadError: message, connecting: false };
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