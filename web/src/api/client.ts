// The HTTP client: one place that knows the frozen wire contract
// (docs/contracts/api.md), the auth token and the error envelope.
//
// Two rules it exists to enforce:
//
//   * The token is read from ?token= exactly once, kept in sessionStorage (not
//     localStorage: a token must not survive the tab), and then stripped from
//     the address bar with history.replaceState — a token in the URL leaks into
//     screenshots, history and the Referer header.
//   * Every /api/* response is checked for the protocol version and for the
//     `{error:{code,message}}` envelope, so callers branch on a code and never
//     on a status number.

import { decodeFrame, HeaderVersion, IdempotencyKeyHeader, Version } from './protocol';
import { parseReport, type CapabilityReport } from './capabilities';
import { parseBoards, type TeamBoardReport } from './teams';
import { parseGoal, type Goal } from './goal';
// Its parser has its own name because `parseReport` is already taken twice
// above and a third import of that name would silently keep the last one.
import { parseTree, type SubagentTree } from './subagents';
// Aliased: `capabilities` and `presents` both export a `parseReport`, and they
// parse different answers. Two imports of one name would silently keep the last.
import { parseReport as parsePresentReport, type PresentReport } from './presents';
import type {
  ApprovalDecision,
  ApprovalDecisionResponse,
  Attachment,
  CancelResponse,
  EventsPage,
  PingFrame,
  PostMessageResponse,
  ServerFrame,
  SessionDetail,
  SessionList,
  SteerResponse,
} from './types';

/** The error envelope's machine-readable half, surfaced as a throwable. */
export class ApiError extends Error {
  readonly code: string;
  readonly status: number;
  readonly details: Record<string, unknown> | undefined;

  constructor(code: string, message: string, status: number, details?: Record<string, unknown>) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = status;
    this.details = details;
  }
}

export interface ClientOptions {
  /** Origin the API lives on; '' means same origin (the shipped UI). */
  baseUrl?: string;
  token: string;
  protocolVersion?: number;
  fetchImpl?: typeof fetch;
}

export interface ListSessionsOptions {
  cursor?: string;
  limit?: number;
}

/**
 * What `postMessage` puts in `attachments`.
 *
 * It is the protocol's Attachment plus the two optional members the server's
 * own struct declares — `mediaType` and `name` at
 * crates/protocol/src/protocol.rs:180-193, both `Option<String>` and both
 * recorded per attachment (`server.rs:2021-2022`). The generated
 * `protocol.ts` predates them and is owned elsewhere, so they are added here
 * rather than restated there: this is the `Attachment` type, not a copy of it,
 * and an older server simply ignores what it does not read. They are what lets
 * a transcript say "shot.png (image/png)" instead of "an image".
 */
export type OutgoingAttachment = Attachment & { name?: string; mediaType?: string };

/** The subset of the client the session screen needs, so tests can fake it. */
export interface HearthApi {
  listSessions(options?: ListSessionsOptions): Promise<SessionList>;
  getSession(id: string): Promise<SessionDetail>;
  events(id: string, after: number, limit?: number): Promise<EventsPage>;
  postMessage(
    id: string,
    text: string,
    attachments?: readonly OutgoingAttachment[],
  ): Promise<PostMessageResponse>;
  /** Ask the running turn to stop (§4.9); the turn's ending arrives on the stream. */
  cancel(id: string): Promise<CancelResponse>;
  /** Inject text into the running turn (§4.10); 409 when nothing is running. */
  steer(id: string, text: string): Promise<SteerResponse>;
  decide(
    id: string,
    approvalId: string,
    decision: ApprovalDecision,
    note?: string,
  ): Promise<ApprovalDecisionResponse>;
  /** What this process was configured with; one GET, no session involved. */
  capabilities(): Promise<CapabilityReport>;
  /** Team boards `hearth team run` left in the data dir; one GET, no session. */
  teams(): Promise<TeamBoardReport>;
  /** The goal a session pursues, or null when it pursues none. */
  getGoal(id: string): Promise<Goal | null>;
  /** Files the model handed over with `present`; one GET, no session. */
  presents(): Promise<PresentReport>;
  /** The subagent tree of one session, joined with each child's timing. */
  subagents(id: string): Promise<SubagentTree>;
}

function trimTrailingSlash(baseUrl: string): string {
  return baseUrl.endsWith('/') ? baseUrl.slice(0, -1) : baseUrl;
}

/** A fresh key for a state-changing POST that must not append twice on retry. */
function newIdempotencyKey(): string {
  return typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `${String(Date.now())}-${Math.random().toString(36).slice(2)}`;
}

export class HearthClient implements HearthApi {
  readonly baseUrl: string;
  readonly token: string;
  readonly protocolVersion: number;
  private readonly fetchImpl: typeof fetch;

  constructor(options: ClientOptions) {
    this.baseUrl = trimTrailingSlash(options.baseUrl ?? '');
    this.token = options.token;
    this.protocolVersion = options.protocolVersion ?? Version;
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
  }

  private headers(extra: Record<string, string> = {}): Record<string, string> {
    return {
      Accept: 'application/json',
      [HeaderVersion]: String(this.protocolVersion),
      ...(this.token === '' ? {} : { Authorization: `Bearer ${this.token}` }),
      ...extra,
    };
  }

  private async request<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      ...init,
      headers: this.headers({
        ...(init.body === undefined ? {} : { 'Content-Type': 'application/json' }),
        ...((init.headers as Record<string, string> | undefined) ?? {}),
      }),
    });

    if (!response.ok) {
      // The envelope is {"error":{"code","message","details"?}}; a proxy or a
      // crashed server may return something else, which must not crash the UI.
      let code = `http_${String(response.status)}`;
      let message = response.statusText === '' ? 'request failed' : response.statusText;
      let details: Record<string, unknown> | undefined;
      try {
        const body: unknown = await response.json();
        if (typeof body === 'object' && body !== null && 'error' in body) {
          const error = (body as { error?: unknown }).error;
          if (typeof error === 'object' && error !== null) {
            const e = error as { code?: unknown; message?: unknown; details?: unknown };
            if (typeof e.code === 'string') code = e.code;
            if (typeof e.message === 'string') message = e.message;
            if (typeof e.details === 'object' && e.details !== null) {
              details = e.details as Record<string, unknown>;
            }
          }
        }
      } catch {
        // Keep the synthetic code from the status line.
      }
      throw new ApiError(code, message, response.status, details);
    }

    const echoed = response.headers.get(HeaderVersion);
    if (echoed !== null && echoed !== String(this.protocolVersion)) {
      throw new ApiError(
        'protocol_mismatch',
        `server answered protocol ${echoed}, this client speaks ${String(this.protocolVersion)}`,
        response.status,
      );
    }
    return (await response.json()) as T;
  }

  listSessions(options: ListSessionsOptions = {}): Promise<SessionList> {
    const query = new URLSearchParams();
    if (options.cursor !== undefined && options.cursor !== '') query.set('cursor', options.cursor);
    if (options.limit !== undefined) query.set('limit', String(options.limit));
    const suffix = query.toString();
    return this.request<SessionList>(`/api/sessions${suffix === '' ? '' : `?${suffix}`}`);
  }

  getSession(id: string): Promise<SessionDetail> {
    return this.request<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
  }

  events(id: string, after: number, limit = 200): Promise<EventsPage> {
    const query = new URLSearchParams({ after: String(after), limit: String(limit) });
    return this.request<EventsPage>(`/api/sessions/${encodeURIComponent(id)}/events?${query.toString()}`);
  }

  postMessage(
    id: string,
    text: string,
    attachments: readonly OutgoingAttachment[] = [],
  ): Promise<PostMessageResponse> {
    // The key makes a retry safe: the same key never appends a second message.
    // `attachments` is left out entirely when there are none: the field is
    // optional, and a message without files must stay byte-identical to what it
    // was before attachments existed (api/client.test.ts asserts that body). An
    // empty array would be a different request for no gain.
    const body = attachments.length === 0 ? { text } : { text, attachments };
    return this.request<PostMessageResponse>(`/api/sessions/${encodeURIComponent(id)}/messages`, {
      method: 'POST',
      headers: { [IdempotencyKeyHeader]: newIdempotencyKey() },
      body: JSON.stringify(body),
    });
  }

  cancel(id: string): Promise<CancelResponse> {
    // No body and no key: cancelling twice is the same request, and the answer
    // says whether a turn was open in the log at the time (§4.9).
    return this.request<CancelResponse>(`/api/sessions/${encodeURIComponent(id)}/cancel`, {
      method: 'POST',
    });
  }

  steer(id: string, text: string): Promise<SteerResponse> {
    return this.request<SteerResponse>(`/api/sessions/${encodeURIComponent(id)}/steer`, {
      method: 'POST',
      headers: { [IdempotencyKeyHeader]: newIdempotencyKey() },
      body: JSON.stringify({ text }),
    });
  }

  decide(
    id: string,
    approvalId: string,
    decision: ApprovalDecision,
    note?: string,
  ): Promise<ApprovalDecisionResponse> {
    const body = note === undefined || note.trim() === '' ? { decision } : { decision, note };
    return this.request<ApprovalDecisionResponse>(
      `/api/sessions/${encodeURIComponent(id)}/approvals/${encodeURIComponent(approvalId)}`,
      { method: 'POST', body: JSON.stringify(body) },
    );
  }

  /**
   * Save an edited buffer into a session's workspace (§4.20).
   *
   * Deliberately not part of `HearthApi`: that interface is what a session view
   * needs, and every one of its fakes would have to grow a method none of them
   * call. The workbench has its own seam (`WorkbenchApi`), and this is behind
   * it.
   *
   * No `Idempotency-Key`: a save is not retried by the transport, and a retry
   * that did land twice would be caught by `expectedSha256` on the second
   * attempt — the digest of the bytes the buffer was built from no longer
   * matches the file, which is exactly the case that key protects against on
   * `postMessage`. The answer carries the new digest so the editor can keep
   * saving without reloading.
   */
  saveFile(
    id: string,
    path: string,
    text: string,
    expectedSha256?: string,
  ): Promise<{ path: string; bytes: number; sha256: string }> {
    const body =
      expectedSha256 === undefined || expectedSha256 === ''
        ? { path, text }
        : { path, text, expectedSha256 };
    return this.request(`/api/sessions/${encodeURIComponent(id)}/file`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  }

  capabilities(): Promise<CapabilityReport> {
    // Decoded through parseReport rather than cast: this endpoint is newer than
    // the frozen protocol module, so an older or partial server must degrade to
    // empty groups instead of handing the panel `undefined`.
    return this.request<unknown>('/api/capabilities').then(parseReport);
  }

  teams(): Promise<TeamBoardReport> {
    // Same reason as `capabilities`: newer than the protocol module, so the
    // answer is parsed defensively. A raise here would read as "the routes are
    // broken" when the truth is "nothing has run a team yet".
    return this.request<unknown>('/api/teams').then(parseBoards);
  }

  getGoal(id: string): Promise<Goal | null> {
    // `null` is the sentence "this session pursues no goal", which is a fact
    // about an empty log, not a fault to report. Parsed for the same reason as
    // `capabilities`: the route is newer than the frozen protocol module.
    return this.request<unknown>(`/api/sessions/${encodeURIComponent(id)}/goal`).then(parseGoal);
  }

  presents(): Promise<PresentReport> {
    // Same reason as `teams`: newer than the frozen protocol module, so the
    // answer is parsed defensively. Raising here would read as "the route is
    // broken" when the truth is "nothing has been presented yet".
    return this.request<unknown>('/api/presents').then(parsePresentReport);
  }

  subagents(id: string): Promise<SubagentTree> {
    // Parsed, not cast: the route is newer than the frozen protocol module, so
    // a server that does not know it answers 404 and this degrades to an empty
    // tree instead of leaving the panel with `undefined`.
    return this.request<unknown>(
      `/api/sessions/${encodeURIComponent(id)}/subagents`,
    ).then(parseTree);
  }

  /**
   * GET one JSON document under `/api/...`, through the same envelope and
   * protocol-version checks every other method uses.
   *
   * It exists so that api/workbench.ts (the `/files`, `/file` and `/diff`
   * routes) can be written without a second fetch wrapper: the error envelope,
   * the version echo and the Bearer header are decided here, once. Callers
   * still have to parse the answer — this returns `unknown` on purpose.
   */
  getJson<T>(path: string): Promise<T> {
    return this.request<T>(path);
  }
}

/**
 * The WebSocket subprotocol that carries the protocol version. A page cannot
 * set `X-Hearth-Protocol` on a handshake, so the version rides here and the
 * server echoes the choice in the 101 (RFC 6455 §4.2.2). The token cannot ride
 * here too: the server sets the HttpOnly `hearth_token` cookie when it serves the
 * shell for the printed URL.
 */
export const WS_SUBPROTOCOL = 'hearth.v1';

/** WebSocket URL for a session stream, same origin unless baseUrl overrides. */
export function streamUrl(baseUrl: string, sessionId: string, after: number): string {
  let origin = baseUrl;
  if (origin === '') {
    origin =
      typeof location === 'undefined'
        ? 'http://127.0.0.1'
        : `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}`;
  } else if (origin.startsWith('http://')) {
    origin = `ws://${origin.slice('http://'.length)}`;
  } else if (origin.startsWith('https://')) {
    origin = `wss://${origin.slice('https://'.length)}`;
  }
  const query = new URLSearchParams({ after: String(after) });
  return `${origin}/api/sessions/${encodeURIComponent(sessionId)}/stream?${query.toString()}`;
}

export const TOKEN_STORAGE_KEY = 'hearth.token';

export interface TokenCaptureSources {
  /** Location-like object; defaults to the real window.location. */
  location?: { href: string; search: string; pathname: string; hash: string };
  storage?: Pick<Storage, 'getItem' | 'setItem'>;
  replaceState?: (url: string) => void;
}

/**
 * Read the token from ?token= once, persist it for this tab only and remove it
 * from the address bar. Returns '' when there is no token, which the UI treats
 * as "not authenticated yet".
 */
export function captureToken(sources: TokenCaptureSources = {}): string {
  const locationLike = sources.location ?? (typeof location === 'undefined' ? null : location);
  const storage =
    sources.storage ??
    (typeof sessionStorage === 'undefined' ? null : sessionStorage);
  const replaceState =
    sources.replaceState ??
    (typeof history === 'undefined'
      ? undefined
      : (url: string) => {
          history.replaceState(history.state, '', url);
        });

  if (locationLike !== null) {
    const params = new URLSearchParams(locationLike.search);
    const fromUrl = params.get('token');
    if (fromUrl !== null && fromUrl !== '') {
      storage?.setItem(TOKEN_STORAGE_KEY, fromUrl);
      params.delete('token');
      const query = params.toString();
      const stripped = `${locationLike.pathname}${query === '' ? '' : `?${query}`}${locationLike.hash}`;
      replaceState?.(stripped);
      return fromUrl;
    }
  }

  const stored = storage?.getItem(TOKEN_STORAGE_KEY) ?? '';
  return stored;
}

/** Parse one WS payload into a protocol frame (throws on garbage). */
export function parseFrame(raw: string): ServerFrame {
  return decodeFrame(raw);
}

export const PING_FRAME: PingFrame = { kind: 'ping' };