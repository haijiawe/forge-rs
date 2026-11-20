// The workbench routes: one directory listing, one file's text, one diff list.
//
//   GET /api/sessions/{id}/files?path=<relative, empty = root>
//   GET /api/sessions/{id}/file?path=<relative>
//   GET /api/sessions/{id}/diff
//
// Two rules, both inherited from api/capabilities.ts:
//
//   * The transport is client.ts's own `getJson`, never a second fetch wrapper.
//     The error envelope, the protocol-version echo and the Bearer header are
//     decided in one place, and `bad_request` therefore reaches the caller as an
//     ApiError with a code, not as a bare 400.
//   * The answers are parsed, not cast. These routes are newer than the frozen
//     protocol module, so a partial or older server must degrade to an empty
//     pane instead of handing a component `undefined`.
//
// The helpers take no client because that is the agreed contract
// (`listDir(sessionId, path)`), so the client is module-level and injectable:
// production leaves it unset and gets one built from the tab's token (or from
// nothing at all, which leaves the HttpOnly cookie to authenticate, exactly as
// the shell's own client does); tests point it at a fake fetch.

import { captureToken, HearthClient } from './client';

export interface FileEntry {
  name: string;
  kind: 'file' | 'dir';
  /** Files only; a directory has no size. */
  size?: number;
}

export interface DirListing {
  path: string;
  entries: FileEntry[];
}

export interface FileText {
  path: string;
  size: number;
  text: string;
  /** The server caps a file at 1 MiB and says so here. */
  truncated: boolean;
  /** Binary files answer `text: ""` and this flag. */
  binary: boolean;
}

export interface DiffEntry {
  path: string;
  status: string;
  additions: number;
  deletions: number;
}

let injected: HearthClient | null = null;
let fallback: HearthClient | null = null;

/**
 * Point these helpers at a client, or pass null to restore the default.
 *
 * The default is built lazily, once: a token from sessionStorage when the URL
 * carried one, otherwise none — client.ts omits the Authorization header then,
 * which is what lets the server's HttpOnly cookie answer for the page.
 */
export function setWorkbenchClient(client: HearthClient | null): void {
  injected = client;
}

function http(): HearthClient {
  if (injected !== null) return injected;
  fallback ??= new HearthClient({ token: captureToken() });
  return fallback;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : null;
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function count(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

function sessionPath(sessionId: string, suffix: string): string {
  return `/api/sessions/${encodeURIComponent(sessionId)}${suffix}`;
}

/** One directory, as the server listed it. Empty path means the workspace root. */
export async function listDir(sessionId: string, path: string): Promise<DirListing> {
  // An empty `path` is the root and is left off the query rather than sent as
  // `path=`: both mean the same to the server, and the shorter URL is the one a
  // log line shows.
  const query = path === '' ? '' : `?path=${encodeURIComponent(path)}`;
  const value = await http().getJson<unknown>(sessionPath(sessionId, `/files${query}`));
  return parseListing(value, path);
}

/** One file's text. `path` must be relative to the session's workspace. */
export async function readFile(sessionId: string, path: string): Promise<FileText> {
  const value = await http().getJson<unknown>(
    sessionPath(sessionId, `/file?path=${encodeURIComponent(path)}`),
  );
  return parseFile(value, path);
}

/** The working tree's changes. `[]` means "no git repository, or nothing changed". */
export async function listDiff(sessionId: string): Promise<DiffEntry[]> {
  const value = await http().getJson<unknown>(sessionPath(sessionId, '/diff'));
  return parseDiff(value);
}

/** Decode `{path, entries[]}`; unknown entries are skipped, not guessed at. */
export function parseListing(value: unknown, requested = ''): DirListing {
  const body = asRecord(value);
  const entries: FileEntry[] = [];
  const raw = body?.entries;
  if (Array.isArray(raw)) {
    for (const item of raw) {
      const entry = asRecord(item);
      if (entry === null) continue;
      const name = text(entry.name);
      if (name === '') continue;
      const kind = entry.kind === 'dir' ? 'dir' : entry.kind === 'file' ? 'file' : null;
      // An unknown `kind` is data from a newer server; skipping it keeps the
      // tree drawable, and the entry is counted nowhere rather than mislabelled.
      if (kind === null) continue;
      const size = typeof entry.size === 'number' && Number.isFinite(entry.size) ? entry.size : undefined;
      entries.push(size === undefined ? { name, kind } : { name, kind, size });
    }
  }
  const echo = text(body?.path);
  return { path: echo === '' ? requested : echo, entries };
}

/**
 * Decode `{path, size, text, truncated, binary}`.
 *
 * Unlike the two lists this does not degrade to empty: a listing that cannot be
 * read still has a tree to show, but a file whose `text` is missing is a failed
 * read, and a blank editor would look like an empty file. So it throws with the
 * path in the sentence, and the pane shows that instead.
 */
export function parseFile(value: unknown, requested = ''): FileText {
  const body = asRecord(value);
  if (body === null || typeof body.text !== 'string') {
    throw new Error(`the server answered no text for ${requested === '' ? 'that path' : requested}`);
  }
  const echo = text(body.path);
  return {
    path: echo === '' ? requested : echo,
    size: count(body.size),
    text: body.text,
    truncated: body.truncated === true,
    binary: body.binary === true,
  };
}

/** Decode `{files[]}`; an answer without the key is the empty list, not an error. */
export function parseDiff(value: unknown): DiffEntry[] {
  const body = asRecord(value);
  const raw = body?.files;
  if (!Array.isArray(raw)) return [];
  const files: DiffEntry[] = [];
  for (const item of raw) {
    const entry = asRecord(item);
    if (entry === null) continue;
    const path = text(entry.path);
    if (path === '') continue;
    files.push({
      path,
      // Left as the server sent it: `status` is a git word ("modified",
      // "untracked", …) and this module has no business translating it into a
      // word of its own invention.
      status: text(entry.status),
      additions: count(entry.additions),
      deletions: count(entry.deletions),
    });
  }
  return files;
}