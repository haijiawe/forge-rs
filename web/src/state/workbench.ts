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
import { listDir, listDiff, readFile } from '../api/workbench';
import type { DiffEntry, DirListing, FileText } from '../api/workbench';

/** The three reads the workbench performs, faked whole in tests. */
export interface WorkbenchApi {
  listDir(sessionId: string, path: string): Promise<DirListing>;
  readFile(sessionId: string, path: string): Promise<FileText>;
  listDiff(sessionId: string): Promise<DiffEntry[]>;
}

/** The real HTTP module; a module constant so a prop default keeps its identity. */
export const defaultWorkbenchApi: WorkbenchApi = { listDir, readFile, listDiff };

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
    const tab: FileTab = { path, file: null, loading: false, error: null };
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

  /** Close a tab; the neighbour takes the focus when it was the active one. */
  closeFile(path: string): void {
    const index = this.state.open.findIndex((tab) => tab.path === path);
    if (index < 0) return;
    const open = this.state.open.filter((tab) => tab.path !== path);
    const activePath =
      this.state.activePath !== path
        ? this.state.activePath
        : (open[index]?.path ?? open[index - 1]?.path ?? null);
    this.publish({ open, activePath });
  }

  setActive(path: string): void {
    // Only an open tab can take the focus: the tree highlights the active file,
    // and a path that is not in `open` would leave the tabs and the tree
    // disagreeing about what is on screen.
    if (path === this.state.activePath) return;
    if (!this.state.open.some((tab) => tab.path === path)) return;
    this.publish({ activePath: path });
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
  }

  /** Re-read a directory, e.g. after the agent wrote a file. */
  refreshDir(path: string): void {
    void this.loadDir(path);
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
      this.patchTab(path, { file, loading: false, error: null });
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

  private patchTab(path: string, patch: Partial<FileTab>): void {
    this.publish({
      open: this.state.open.map((tab) => (tab.path === path ? { ...tab, ...patch } : tab)),
    });
  }

  private publish(change: Partial<WorkbenchState>): void {
    this.version += 1;
    this.state = { ...this.state, ...change, version: this.version };
    for (const listener of this.listeners) listener();
  }
}