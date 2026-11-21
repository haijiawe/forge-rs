// The workbench's own state: which directories are open, which files are in the
// tabs, and whether the centre pane shows the diff list instead of the editor.
//
// Deliberately small and synchronous. The session store needs a stream machine
// and animation-frame batching because frames arrive many times per second; a
// directory listing arrives once and then stops. So this store publishes on
// every change, with no timer, socket or flush to reason about.
//
// It never invents a row. A directory that has not answered is `loading`, one
// that answered with nothing keeps `entries: []`, and a failure keeps its
// message; the views decide which of the three to draw. That is what keeps the
// "empty" text a statement about the workspace rather than about the network.

import { ApiError } from '../api/client';
import {
  listDir,
  listDiff,
  readDiff,
  readFile,
  revertFile,
  searchFiles,
  writeFile,
} from '../api/workbench';
import type {
  DiffEntry,
  DirListing,
  FileDiff,
  FileText,
  SaveResult,
  SearchAnswer,
} from '../api/workbench';
import { parseChanged } from './changed';
import type { ChangedLines } from './changed';

/** The reads and the one write the workbench performs, faked whole in tests. */
export interface WorkbenchApi {
  listDir(sessionId: string, path: string): Promise<DirListing>;
  readFile(sessionId: string, path: string): Promise<FileText>;
  listDiff(sessionId: string): Promise<DiffEntry[]>;
  readDiff(sessionId: string, path: string): Promise<FileDiff>;
  saveFile(sessionId: string, path: string, text: string, expectedSha256: string): Promise<SaveResult>;
  revertFile(sessionId: string, path: string): Promise<SaveResult>;
  searchFiles(sessionId: string, query: string): Promise<SearchAnswer>;
}

/** The real HTTP module; a module constant so a prop default keeps its identity. */
export const defaultWorkbenchApi: WorkbenchApi = {
  listDir,
  readFile,
  listDiff,
  readDiff,
  saveFile: writeFile,
  revertFile,
  searchFiles,
};

/** One directory in the tree, keyed by its relative path ('' is the root). */
export interface DirState {
  listing: DirListing | null;
  loading: boolean;
  error: string | null;
}

/** One open tab. `file` is null until the read answers, or forever on failure. */
export interface FileTab {
  path: string;
  file: FileText | null;
  loading: boolean;
  error: string | null;
  /**
   * The buffer, once it differs from `file.text`.
   *
   * Edits live here rather than being written into `file.text`: `file` is the
   * server's answer, and the moment the two are the same field a save cannot
   * tell "nothing changed" from "everything changed". `null` means clean, which
   * is also the state right after a successful save.
   */
  draft: string | null;
  /** True while a save is in flight; a second Ctrl+S must not race the first. */
  saving: boolean;
  /** The last save's failure, cleared by the next attempt or a reload. */
  saveError: string | null;
}

/**
 * Where the open file and the commit disagree.
 *
 * It exists for the editor's line marks: a reader who opens a file the agent just
 * wrote should see which lines are new without switching to the diff list. It is
 * keyed by path because the answer belongs to the file, not to the pane, and a
 * stale answer for another path must not be drawn over this one.
 */
export interface ChangesState {
  path: string;
  /** Null until the read answers, and after a failure; `error` says which. */
  lines: ChangedLines | null;
  /** True when the server cut the diff short, so the marks are incomplete. */
  truncated: boolean;
  loading: boolean;
  error: string | null;
}

/** One expanded row of the diff list: its unified diff, once it has answered. */
export interface FileDiffState {
  path: string;
  body: FileDiff | null;
  loading: boolean;
  error: string | null;
}

export interface WorkbenchState {
  /** Monotonic; useSyncExternalStore subscribers diff on this. */
  version: number;
  sessionId: string;
  /** Expanded directory paths, sorted. The root is always drawn, so it is not here. */
  expanded: readonly string[];
  dirs: Readonly<Record<string, DirState>>;
  /** Open tabs, in the order they were opened. */
  open: readonly FileTab[];
  activePath: string | null;
  showDiff: boolean;
  diff: readonly DiffEntry[] | null;
  diffLoading: boolean;
  diffError: string | null;
  /**
   * The diff row whose lines are showing, or null when every row is collapsed.
   *
   * One at a time, because two diffs open at once would scroll independently and
   * the reader would have to remember which half of the pane they were in.
   */
  fileDiff: FileDiffState | null;
  /**
   * The line marks for the active tab, or null when the question has not been
   * asked yet (no tab, or the read is still in flight with nothing to show).
   */
  changes: ChangesState | null;
  /**
   * A tab the operator asked to close while it had unsaved changes.
   *
   * The close is held here instead of thrown away so the pane can ask. Nothing
   * reaches the disk either way, but a draft that disappears on a stray click
   * is work the operator cannot get back, and the log has no record of it.
   */
  pendingClose: string | null;
  /**
   * The last workspace search, or null when the search box is empty again.
   *
   * Kept here rather than in the component so that opening a result — which
   * switches the centre pane to the editor — does not throw the result list
   * away: the operator is usually reading the fourth of twelve hits.
   */
  search: SearchState | null;
  /**
   * A line the editor should scroll to, set by opening a search hit.
   *
   * It is paired with a path because a reveal that outlives the file it was
   * about would jump the reader to line 40 of a different file. The editor
   * ignores it unless the path matches the tab it is drawing.
   */
  reveal: { path: string; line: number } | null;
}

/** One search: the query, whether it is still running, and what came back. */
export interface SearchState {
  query: string;
  loading: boolean;
  error: string | null;
  answer: SearchAnswer | null;
}

export interface WorkbenchOptions {
  sessionId: string;
  api?: WorkbenchApi;
}

function errorMessage(error: unknown): string {
  // Same shape as the session store's: an ApiError is "code: message" so the
  // operator can tell `bad_request` from `http_500` at a glance.
  if (error instanceof ApiError) return `${error.code}: ${error.message}`;
  if (error instanceof Error) return error.message;
  return String(error);
}

export class WorkbenchStore {
  private readonly api: WorkbenchApi;
  private readonly listeners = new Set<() => void>();
  private state: WorkbenchState;
  private version = 0;
  private started = false;

  constructor(options: WorkbenchOptions) {
    this.api = options.api ?? defaultWorkbenchApi;
    this.state = {
      version: 0,
      sessionId: options.sessionId,
      expanded: [],
      dirs: {},
      open: [],
      activePath: null,
      showDiff: false,
      diff: null,
      diffLoading: false,
      diffError: null,
      fileDiff: null,
      changes: null,
      pendingClose: null,
      search: null,
      reveal: null,
    };
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): WorkbenchState => this.state;

  /** Read the workspace root. One call; a second start is a no-op. */
  start(): void {
    if (this.started) return;
    this.started = true;
    void this.loadDir('');
  }

  /** Expand a directory (reading it the first time) or collapse it again. */
  toggleDir(path: string): void {
    const expanded = this.state.expanded;
    if (expanded.includes(path)) {
      this.publish({ expanded: expanded.filter((entry) => entry !== path) });
      return;
    }
    this.publish({ expanded: [...expanded, path].sort() });
    // A listing already in hand is reused: collapsing a directory and opening it
    // again must not re-walk the disk. A failed read left `listing: null`, so
    // reopening it is also how it is retried.
    const known = this.state.dirs[path];
    if (known === undefined || known.listing === null) void this.loadDir(path);
  }

  /** Focus a tab, opening and reading the file when it is not open yet. */
  openFile(path: string): void {
    if (path === '') return;
    const existing = this.state.open.find((tab) => tab.path === path);
    if (existing !== undefined) {
      this.publish({ activePath: path });
      if (existing.file === null) void this.loadFile(path);
      return;
    }
    const tab: FileTab = {
      path,
      file: null,
      loading: false,
      error: null,
      draft: null,
      saving: false,
      saveError: null,
    };
    this.publish({ open: [...this.state.open, tab], activePath: path });
    void this.loadFile(path);
  }

  /**
   * Open a file named by a diff row and leave the diff view.
   *
   * Switching back is part of the gesture: a row that opens a file behind a list
   * that stays on screen looks like a click that did nothing.
   */
  openFromDiff(path: string): void {
    this.publish({ showDiff: false });
    this.openFile(path);
  }

  /**
   * Expand one row's lines, or collapse it when it is already the open one.
   *
   * The row is not re-read on collapse-then-expand: a diff that has answered is
   * kept, and `refreshDiff` is how a stale one is replaced. The count in the row
   * and the lines below it can therefore disagree for a moment after the agent
   * writes something, which is why the row shows the count and the pane shows
   * the bytes rather than one number derived from the other.
   */
  toggleFileDiff(path: string): void {
    const current = this.state.fileDiff;
    if (current !== null && current.path === path) {
      this.publish({ fileDiff: null });
      return;
    }
    this.publish({ fileDiff: { path, body: null, loading: true, error: null } });
    void this.loadFileDiff(path);
  }

  /** Re-read the expanded row after a failure, leaving it expanded. */
  retryFileDiff(path: string): void {
    this.publish({ fileDiff: { path, body: null, loading: true, error: null } });
    void this.loadFileDiff(path);
  }

  /** Collapse the expanded row (the pane's close button). */
  hideFileDiff(): void {
    if (this.state.fileDiff === null) return;
    this.publish({ fileDiff: null });
  }

  /**
   * Close a tab; the neighbour takes the focus when it was the active one.
   *
   * A tab with unsaved changes is not closed here: it is parked in
   * `pendingClose` so the pane can ask. Every other path through this method
   * (a clean tab, a failed read, a binary file nobody can edit) closes at once,
   * because a confirmation on work that cannot be lost is noise that trains the
   * operator to click through the one that matters.
   */
  closeFile(path: string): void {
    const tab = this.state.open.find((entry) => entry.path === path);
    if (tab === undefined) return;
    if (tab.draft !== null) {
      this.publish({ pendingClose: path });
      return;
    }
    this.closeNow(path);
  }

  /** Throw the parked draft away and close the tab. */
  confirmClose(): void {
    const path = this.state.pendingClose;
    if (path === null) return;
    this.publish({ pendingClose: null });
    this.closeNow(path);
  }

  /** Keep the draft; the tab stays open and the question goes away. */
  cancelClose(): void {
    if (this.state.pendingClose === null) return;
    this.publish({ pendingClose: null });
  }

  setActive(path: string): void {
    // Only an open tab can take the focus: the tree highlights the active file,
    // and a path that is not in `open` would leave the tabs and the tree
    // disagreeing about what is on screen.
    if (path === this.state.activePath) return;
    if (!this.state.open.some((tab) => tab.path === path)) return;
    this.publish({ activePath: path });
  }

  /**
   * Record an edit. Nothing reaches the disk until `save` is called.
   *
   * Typing back to the exact bytes that were read clears the draft rather than
   * keeping an identical copy: a tab that says "unsaved" while its text equals
   * the file is a lie about work that does not exist.
   */
  editFile(path: string, text: string): void {
    const tab = this.state.open.find((entry) => entry.path === path);
    if (tab === undefined || tab.file === null) return;
    if (tab.draft === text) return;
    this.patchTab(path, { draft: text === tab.file.text ? null : text, saveError: null });
  }

  /**
   * Drop the draft and go back to the bytes that were read.
   *
   * This is the cheap undo — it touches no disk and asks no server. Restoring
   * the *committed* version is `revertToCommitted`, a different thing with a
   * different name, because "revert" means both and only one of them writes.
   */
  discardDraft(path: string): void {
    this.patchTab(path, { draft: null, saveError: null });
  }

  /**
   * Ask the server to put the file back to its committed bytes.
   *
   * Returns the reason it was refused, or null. The message is returned rather
   * than only stored on the tab because the caller is usually the diff list,
   * where the file may not be open at all — a failure recorded only in a tab
   * that is not on screen is a failure nobody reads.
   *
   * Refused while the tab has a draft: the draft is unsaved work that exists
   * only in this tab, and restoring first would throw it away while the operator
   * was looking at a button labelled "restore", not "discard".
   */
  async revertToCommitted(path: string): Promise<string | null> {
    const tab = this.state.open.find((entry) => entry.path === path);
    if (tab !== undefined && tab.saving) return '这个文件正在保存，等它结束再还原。';
    if (tab !== undefined && tab.draft !== null) {
      const refusal = '这个标签有未保存的改动：先保存或丢弃，再还原到上次提交。';
      this.patchTab(path, { saveError: refusal });
      return refusal;
    }
    if (tab !== undefined) this.patchTab(path, { saving: true, saveError: null });
    try {
      const restored = await this.api.revertFile(this.state.sessionId, path);
      // The restore answers with a digest and a size, not the text: the pane
      // needs the bytes to show them, and re-reading is the same call the tab
      // would make if it were opened now.
      const fresh = await this.api.readFile(this.state.sessionId, path);
      const live = this.state.open.find((entry) => entry.path === path);
      if (live !== undefined && live.file !== null) {
        this.patchTab(path, {
          file: {
            ...live.file,
            text: fresh.text,
            size: restored.bytes,
            sha256: restored.sha256,
            truncated: fresh.truncated,
            binary: fresh.binary,
          },
          draft: null,
          saving: false,
          saveError: null,
        });
      }
      // The diff list and the expanded row both describe bytes that just moved;
      // re-reading them is the only way the pane stops showing the change it was
      // just told to undo.
      await this.loadDiff();
      if (this.state.fileDiff?.path === path) await this.loadFileDiff(path);
      // Restored bytes: the marks this file carried described the change that was
      // just thrown away.
      this.loadChanges(path, true);
      return null;
    } catch (error) {
      const refusal = errorMessage(error);
      if (tab !== undefined) this.patchTab(path, { saving: false, saveError: refusal });
      return refusal;
    }
  }

  /**
   * Save every dirty tab, one after another.
   *
   * Sequential rather than parallel: two saves racing through one server put
   * their `file/write` lines in an order nobody chose, and the log is the record
   * of what happened. One failure does not stop the others — the operator asked
   * for all of them, and a list of which ones failed is more useful than the
   * first failure and five untouched tabs.
   */
  async saveAll(): Promise<void> {
    const dirty = this.state.open
      .filter((tab) => tab.draft !== null && !tab.saving)
      .map((tab) => tab.path);
    for (const path of dirty) {
      await this.saveFile(path);
    }
  }

  /**
   * Search the whole workspace, or clear the box when the query is blank.
   *
   * An empty query clears rather than searching: the server refuses it (it would
   * match every line in the tree), and a cleared box should not leave a list of
   * results behind that no longer belongs to anything on screen.
   */
  search(query: string): void {
    const needle = query.trim();
    if (needle === '') {
      this.clearSearch();
      return;
    }
    this.publish({ search: { query: needle, loading: true, error: null, answer: null } });
    void this.runSearch(needle);
  }

  /** Put the search box back to empty, dropping the results with it. */
  clearSearch(): void {
    if (this.state.search === null) return;
    this.publish({ search: null, reveal: null });
  }

  /**
   * Open a search hit: the file in a tab, the diff list out of the way, and the
   * editor scrolled to the line that matched.
   *
   * The results stay on screen — the operator is usually walking down a list,
   * and re-running the search after each click would be work invented for them.
   */
  openHit(path: string, line: number): void {
    if (path === '') return;
    this.publish({ showDiff: false });
    this.openFile(path);
    // `openFile` publishes the tab before the read answers; the reveal is
    // recorded now and the editor applies it whenever the text arrives, which is
    // what makes "open hit" work for a file that is not loaded yet.
    this.publish({ reveal: { path, line } });
  }

  /**
   * Read the active file's marks, or the named one.
   *
   * One request per file switch, and the answer is kept only for the path it was
   * asked about. `force` is for the moments the file itself moved (a save, a
   * restore): the same path may now disagree with the commit differently.
   */
  loadChanges(path: string | null = this.state.activePath, force = false): void {
    if (path === null) {
      if (this.state.changes !== null) this.publish({ changes: null });
      return;
    }
    const known = this.state.changes;
    if (!force && known !== null && known.path === path && (known.loading || known.lines !== null)) {
      return;
    }
    this.publish({
      changes: {
        path,
        lines: known?.path === path ? known.lines : null,
        truncated: known?.path === path ? known.truncated : false,
        loading: true,
        error: null,
      },
    });
    void this.readChanges(path);
  }

  /** Forget the pending reveal, once the editor has scrolled to it. */
  clearReveal(): void {
    if (this.state.reveal === null) return;
    this.publish({ reveal: null });
  }

  /**
   * Write the draft back. A clean tab is a no-op, not an error.
   *
   * The digest of the bytes the buffer was built from rides along as
   * `expectedSha256`, so a file that changed underneath the editor is refused
   * with `conflict` instead of silently overwritten. After a successful save
   * the tab adopts the answer: `file.text` becomes the saved text and its
   * digest becomes the next token, which is what lets a second save work
   * without a reload.
   */
  async saveFile(path: string): Promise<void> {
    const tab = this.state.open.find((entry) => entry.path === path);
    if (tab === undefined || tab.file === null) return;
    if (tab.draft === null || tab.saving) return;
    const draft = tab.draft;
    this.patchTab(path, { saving: true, saveError: null });
    try {
      const saved = await this.api.saveFile(
        this.state.sessionId,
        path,
        draft,
        tab.file.sha256,
      );
      // The tab may have been closed while the request was in flight; writing
      // through `patchTab` on a path that is gone is harmless, but adopting the
      // answer would resurrect nothing, so the update is skipped.
      const live = this.state.open.find((entry) => entry.path === path);
      if (live === undefined || live.file === null) return;
      this.patchTab(path, {
        file: { ...live.file, text: draft, size: saved.bytes, sha256: saved.sha256, truncated: false },
        // Only clear the draft if no further edit arrived during the save;
        // otherwise the newer keystrokes would be replaced by what was sent.
        draft: live.draft === draft ? null : live.draft,
        saving: false,
        saveError: null,
      });
      // The bytes on disk just moved, so the marks in this file are stale. The
      // save is also the moment the operator most wants to see where the lines
      // they just wrote sit relative to the commit.
      this.loadChanges(path, true);
    } catch (error) {
      this.patchTab(path, { saving: false, saveError: errorMessage(error) });
    }
  }

  /** Flip the centre pane between the editor and the diff list, reading once. */
  toggleDiff(): void {
    const showDiff = !this.state.showDiff;
    this.publish({ showDiff });
    if (showDiff && this.state.diff === null && !this.state.diffLoading) void this.loadDiff();
  }

  /** Re-read the diff list after a failure (the pane's retry button). */
  refreshDiff(): void {
    void this.loadDiff();
    // A refresh is the operator saying "what is on screen may be old", so the
    // expanded row is re-read too: leaving it would show fresh counts above
    // stale lines, which reads as a diff that disagrees with itself.
    const open = this.state.fileDiff;
    if (open !== null) {
      this.publish({ fileDiff: { path: open.path, body: null, loading: true, error: null } });
      void this.loadFileDiff(open.path);
    }
  }

  /** Re-read a directory, e.g. after the agent wrote a file. */
  refreshDir(path: string): void {
    void this.loadDir(path);
  }

  private closeNow(path: string): void {
    const index = this.state.open.findIndex((tab) => tab.path === path);
    if (index < 0) return;
    const open = this.state.open.filter((tab) => tab.path !== path);
    const activePath =
      this.state.activePath !== path
        ? this.state.activePath
        : (open[index]?.path ?? open[index - 1]?.path ?? null);
    // The expanded row belongs to the list, not to the tab, so it survives a
    // close; but a parked question about this very tab must not outlive it.
    this.publish({
      open,
      activePath,
      pendingClose: this.state.pendingClose === path ? null : this.state.pendingClose,
    });
  }

  /**
   * One file's unified diff, reduced to the lines that exist on disk and not in
   * the commit.
   *
   * A failure is kept as a message rather than dropped: "no marks" and "could not
   * ask" look identical on screen, and the second one is a statement about the
   * server, not about the file.
   */
  private async readChanges(path: string): Promise<void> {
    try {
      const body = await this.api.readDiff(this.state.sessionId, path);
      if (this.state.changes?.path !== path) return;
      this.publish({
        changes: {
          path,
          lines: parseChanged(body.text),
          truncated: body.truncated,
          loading: false,
          error: null,
        },
      });
    } catch (error) {
      if (this.state.changes?.path !== path) return;
      this.publish({
        changes: { path, lines: null, truncated: false, loading: false, error: errorMessage(error) },
      });
    }
  }

  private async loadFileDiff(path: string): Promise<void> {
    try {
      const body = await this.api.readDiff(this.state.sessionId, path);
      // The operator may have collapsed the row, or expanded another one, while
      // the read was in flight; an answer for a row that is no longer the open
      // one is dropped rather than shown under the wrong path.
      const live = this.state.fileDiff;
      if (live === null || live.path !== path) return;
      this.publish({ fileDiff: { path, body, loading: false, error: null } });
    } catch (error) {
      const live = this.state.fileDiff;
      if (live === null || live.path !== path) return;
      this.publish({ fileDiff: { path, body: null, loading: false, error: errorMessage(error) } });
    }
  }

  private async loadDir(path: string): Promise<void> {
    const known = this.state.dirs[path];
    if (known !== undefined && known.loading) return;
    this.setDir(path, { listing: known?.listing ?? null, loading: true, error: null });
    try {
      const listing = await this.api.listDir(this.state.sessionId, path);
      this.setDir(path, { listing, loading: false, error: null });
    } catch (error) {
      this.setDir(path, { listing: null, loading: false, error: errorMessage(error) });
    }
  }

  private async loadFile(path: string): Promise<void> {
    const known = this.state.open.find((tab) => tab.path === path);
    if (known === undefined || known.loading) return;
    this.patchTab(path, { loading: true, error: null });
    try {
      const file = await this.api.readFile(this.state.sessionId, path);
      // A read replaces the buffer outright: any draft was built on bytes that
      // no longer exist, and a reload is exactly what the conflict message asks
      // the operator to do.
      this.patchTab(path, { file, loading: false, error: null, draft: null, saveError: null });
    } catch (error) {
      this.patchTab(path, { file: null, loading: false, error: errorMessage(error) });
    }
  }

  private async loadDiff(): Promise<void> {
    if (this.state.diffLoading) return;
    this.publish({ diffLoading: true, diffError: null });
    try {
      const files = await this.api.listDiff(this.state.sessionId);
      this.publish({ diff: files, diffLoading: false, diffError: null });
    } catch (error) {
      this.publish({ diff: null, diffLoading: false, diffError: errorMessage(error) });
    }
  }

  private setDir(path: string, next: DirState): void {
    this.publish({ dirs: { ...this.state.dirs, [path]: next } });
  }

  /**
   * Run one search and keep its answer, unless the box moved on meanwhile.
   *
   * The late answer is dropped when the query is no longer the one on screen:
   * results for a query the operator has already replaced are the most
   * convincing kind of wrong, because they look like an answer.
   */
  private async runSearch(query: string): Promise<void> {
    try {
      const answer = await this.api.searchFiles(this.state.sessionId, query);
      if (this.state.search?.query !== query) return;
      this.publish({ search: { query, loading: false, error: null, answer } });
    } catch (error) {
      if (this.state.search?.query !== query) return;
      this.publish({ search: { query, loading: false, error: errorMessage(error), answer: null } });
    }
  }

  private patchTab(path: string, patch: Partial<FileTab>): void {
    this.publish({
      open: this.state.open.map((tab) => (tab.path === path ? { ...tab, ...patch } : tab)),
    });
  }

  private publish(change: Partial<WorkbenchState>): void {
    const previous = this.state.activePath;
    this.version += 1;
    this.state = { ...this.state, ...change, version: this.version };
    for (const listener of this.listeners) listener();
    // Every path into the editor goes through here — a click in the tree, a diff
    // row, a search hit, a closed tab handing the focus to its neighbour — so the
    // marks follow the active file from one place instead of from five call
    // sites, each of which could be forgotten the day a sixth is added.
    if (this.state.activePath !== previous) this.loadChanges();
  }
}