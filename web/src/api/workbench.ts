// The workbench routes: one directory listing, one file's text, one diff list,
// and one save.
//
//   GET /api/sessions/{id}/files?path=<relative, empty = root>
//   GET /api/sessions/{id}/file?path=<relative>
//   GET /api/sessions/{id}/diff
//   PUT /api/sessions/{id}/file   {path, text, expectedSha256?}
//
// The save is the only one that changes anything, and the only one that can be
// refused for a reason the operator has to act on: `conflict` means the file
// moved under the buffer (someone else saved it, or the agent wrote it), and
// the answer is to reload rather than to overwrite. `expectedSha256` is what
// makes that detectable — it is the digest of the bytes the buffer was built
// from, and the server compares it against the file as it is now.
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
  /**
   * Digest of the bytes this text was decoded from, or `""` when the server
   * could not describe them (truncated or binary).
   *
   * Empty is not "unchanged": it is "no token to compare", so a save built on
   * such a read carries no `expectedSha256` and the server accepts it as an
   * ordinary overwrite. That is the honest trade — a token that described only
   * the first megabyte would authorise replacing bytes nobody has looked at.
   */
  sha256: string;
}

/** What a save reports: the bytes it wrote and the digest it produced. */
export interface SaveResult {
  path: string;
  bytes: number;
  sha256: string;
}

export interface DiffEntry {
  path: string;
  status: string;
  additions: number;
  deletions: number;
}

/**
 * One file's unified diff.
 *
 * `text` is git's own format, carried verbatim: this module does not parse it
 * into hunks. A parser here would be a second implementation of a format whose
 * awkward cases — no newline at end of file, binary markers, rename headers —
 * are exactly the ones that matter, and `git` already writes them correctly.
 * The pane colours lines by their first character and nothing else.
 */
export interface FileDiff {
  path: string;
  /** A git word, or `unchanged` for a path whose bytes match the commit. */
  status: string;
  text: string;
  /** The server caps `text` at 1 MiB and says so here. */
  truncated: boolean;
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

/** One file's lines. The same route as `listDiff`, narrowed by `?path=`. */
export async function readDiff(sessionId: string, path: string): Promise<FileDiff> {
  const value = await http().getJson<unknown>(
    sessionPath(sessionId, `/diff?path=${encodeURIComponent(path)}`),
  );
  return parseFileDiff(value, path);
}

/**
 * Save a buffer. `expectedSha256` empty means "I have no token, overwrite".
 *
 * The call can fail with `conflict` (the file moved), `forbidden` (the server
 * was started read-only), `unprocessable` (over 1 MiB) or `bad_request` (the
 * path climbs out of the workspace); each arrives as an ApiError whose `code`
 * the caller switches on, so the pane can say which of the four happened
 * instead of "save failed".
 */
export async function writeFile(
  sessionId: string,
  path: string,
  text: string,
  expectedSha256?: string,
): Promise<SaveResult> {
  const value = await http().saveFile(sessionId, path, text, expectedSha256);
  return parseSave(value, path);
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
    throw new Error(`服务端没有为 ${requested === '' ? '该路径' : requested} 返回任何文本`);
  }
  const echo = text(body.path);
  return {
    path: echo === '' ? requested : echo,
    size: count(body.size),
    text: body.text,
    truncated: body.truncated === true,
    binary: body.binary === true,
    // Absent or empty both mean "no token": an older server never sends it, and
    // a truncated read legitimately has none. Nothing here may invent a digest.
    sha256: text(body.sha256),
  };
}

/**
 * Decode `{path, bytes, sha256}` from a save.
 *
 * It throws when the digest is missing: the editor keeps that value as its next
 * `expectedSha256`, and a save that answered without one would leave the buffer
 * unable to save again without a reload — a failure the operator would meet two
 * keystrokes later, at the worst moment.
 */
export function parseSave(value: unknown, requested = ''): SaveResult {
  const body = asRecord(value);
  const sha256 = text(body?.sha256);
  if (sha256 === '') {
    throw new Error(`服务端保存了 ${requested === '' ? '该文件' : requested}，但没有给出摘要值`);
  }
  const echo = text(body?.path);
  return { path: echo === '' ? requested : echo, bytes: count(body?.bytes), sha256 };
}

/**
 * Decode `{path, status, text, truncated}` from a single-file diff.
 *
 * Unlike `parseDiff` this throws when `text` is missing: an empty string is the
 * honest answer for an unchanged file, so a body without the key is a server
 * that does not know this route yet, and drawing it as "no changes" would be a
 * claim about the workspace the reader cannot check.
 */
export function parseFileDiff(value: unknown, requested = ''): FileDiff {
  const body = asRecord(value);
  if (body === null || typeof body.text !== 'string') {
    throw new Error(`服务端没有为 ${requested === '' ? '该路径' : requested} 返回任何改动内容`);
  }
  const echo = text(body.path);
  return {
    path: echo === '' ? requested : echo,
    status: text(body.status),
    text: body.text,
    truncated: body.truncated === true,
  };
}
/**
 * Throw the buffer away and restore the committed bytes.
 *
 * `bytes` and `sha256` describe what landed on disk, so the pane adopts them as
 * the new baseline exactly as it would after a save. The call fails with
 * `conflict` in the two cases the server refuses — no git repository, or a file
 * git has never committed (restoring it would delete it) — and with `forbidden`
 * when the server was started read-only.
 */
export async function revertFile(sessionId: string, path: string): Promise<SaveResult> {
  const value = await http().revertFile(sessionId, path);
  return parseSave(value, path);
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