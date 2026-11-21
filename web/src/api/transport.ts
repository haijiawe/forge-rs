// The WebSocket transport for one session stream (api.md §4.8).
//
// Transport is deliberately dumb: it opens, decodes frames, forwards them and
// reconnects with ?after=<last applied seq>. Ordering, dedup, gaps and resync
// live in the StreamMachine, because they must be testable without a socket.
//
// Reconnect rule: the cursor is read at connect time from `after()`, so a
// reconnect never re-applies an event the machine already applied (the machine
// also ignores seq <= lastSeq, belt and braces).

import { PING_FRAME, parseFrame, streamUrl, WS_SUBPROTOCOL } from './client';
import type { ClientFrame, ServerFrame } from './types';

export type StreamEvent =
  | { type: 'open' }
  | { type: 'close'; code: number; reason: string }
  | { type: 'frame'; frame: ServerFrame }
  | { type: 'parse-error'; message: string };

export interface StreamOptions {
  sessionId: string;
  token: string;
  /** Last applied seq; called on every connect attempt. */
  after: () => number;
  baseUrl?: string;
  protocolVersion?: number;
  /** Injected in tests; defaults to globalThis.WebSocket. */
  webSocketImpl?: typeof WebSocket;
  /** Keepalive ping interval; 0 disables it. */
  pingIntervalMs?: number;
  reconnect?: { initialDelayMs?: number; maxDelayMs?: number; factor?: number };
  onEvent: (event: StreamEvent) => void;
}

const DEFAULT_RECONNECT = { initialDelayMs: 250, maxDelayMs: 8000, factor: 1.8 };

export class SessionStream {
  private readonly options: StreamOptions;
  private socket: WebSocket | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private closed = false;

  constructor(options: StreamOptions) {
    this.options = options;
  }

  /** Is there a live socket? */
  isOpen(): boolean {
    return this.socket !== null && this.socket.readyState === 1;
  }

  start(): void {
    if (this.closed) return;
    if (this.socket !== null) return;
    const Impl = this.options.webSocketImpl ?? globalThis.WebSocket;
    if (Impl === undefined) {
      this.options.onEvent({ type: 'parse-error', message: 'WebSocket 不可用' });
      return;
    }
    const url = streamUrl(this.options.baseUrl ?? '', this.options.sessionId, this.options.after());
    let socket: WebSocket;
    try {
      // The subprotocol is the only field a page can set on a handshake: both
      // the token (a cookie, set by the server when it serves the shell for the
      // printed URL) and the version (this list) have to travel without headers.
      socket = new Impl(url, [WS_SUBPROTOCOL]);
    } catch (error) {
      this.scheduleReconnect();
      this.options.onEvent({
        type: 'parse-error',
        message: error instanceof Error ? error.message : String(error),
      });
      return;
    }
    this.socket = socket;

    socket.onopen = () => {
      this.attempt = 0;
      this.options.onEvent({ type: 'open' });
      this.startPing();
    };
    socket.onmessage = (event: MessageEvent) => {
      const raw = typeof event.data === 'string' ? event.data : '';
      try {
        const frame = parseFrame(raw);
        if (frame.kind === 'pong') return;
        this.options.onEvent({ type: 'frame', frame });
      } catch (error) {
        this.options.onEvent({
          type: 'parse-error',
          message: error instanceof Error ? error.message : String(error),
        });
      }
    };
    socket.onclose = (event: CloseEvent) => {
      this.socket = null;
      this.stopPing();
      this.options.onEvent({ type: 'close', code: event.code, reason: event.reason });
      this.scheduleReconnect();
    };
    socket.onerror = () => {
      // onclose follows; nothing to do here beyond not crashing.
    };
  }

  /** Send a client frame (ping/pong only; mutations go through HTTP). */
  send(frame: ClientFrame): void {
    if (this.socket !== null && this.socket.readyState === 1) {
      this.socket.send(JSON.stringify(frame));
    }
  }

  close(): void {
    this.closed = true;
    this.stopPing();
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const socket = this.socket;
    this.socket = null;
    if (socket !== null) {
      socket.onclose = null;
      socket.onmessage = null;
      socket.close();
    }
  }

  private startPing(): void {
    const interval = this.options.pingIntervalMs ?? 25_000;
    if (interval <= 0) return;
    this.stopPing();
    this.pingTimer = setInterval(() => {
      this.send(PING_FRAME);
    }, interval);
  }

  private stopPing(): void {
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  private scheduleReconnect(): void {
    if (this.closed) return;
    const config = { ...DEFAULT_RECONNECT, ...(this.options.reconnect ?? {}) };
    const delay = Math.min(
      config.maxDelayMs,
      Math.round(config.initialDelayMs * config.factor ** this.attempt),
    );
    this.attempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.start();
    }, delay);
  }
}