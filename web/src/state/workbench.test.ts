// The workbench store on its own, with a fake API.
//
// These tests pin the four behaviours the views depend on: an empty directory
// stays empty (it is not quietly filled with a placeholder), a directory is
// read once and its listing reused after collapse, a failed file read keeps its
// message instead of producing a blank editor, and the diff list distinguishes
// "not read yet" from "read and empty".
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import type { DiffEntry, DirListing, FileText } from '../api/workbench';
import { WorkbenchStore } from './workbench';
import type { WorkbenchApi } from './workbench';

const SESSION = 's1';

function listing(path: string, entries: DirListing['entries']): DirListing {
  return { path, entries };
}

function textFile(path: string, text: string): FileText {
  return { path, size: text.length, text, truncated: false, binary: false };
}

function fakeApi(overrides: Partial<WorkbenchApi> = {}): WorkbenchApi {
  return {
    listDir: () => Promise.resolve(listing('', [])),
    readFile: (_sessionId, path) => Promise.resolve(textFile(path, '')),
    listDiff: () => Promise.resolve([]),
    ...overrides,
  };
}

/** Let the store's promise chains settle; it publishes synchronously after them. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('WorkbenchStore', () => {
  it('records an empty directory as empty rather than as a failure', async () => {
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi() });
    store.start();
    await settle();

    const root = store.getSnapshot().dirs[''];
    expect(root).toBeDefined();
    expect(root?.loading).toBe(false);
    expect(root?.error).toBeNull();
    expect(root?.listing?.entries).toEqual([]);
  });

  it('reads a directory once and reuses the listing after collapsing', async () => {
    const listDir = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve(
        path === ''
          ? listing('', [{ name: 'src', kind: 'dir' }])
          : listing(path, [{ name: 'main.ts', kind: 'file', size: 12 }]),
      ),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ listDir }) });
    store.start();
    await settle();

    store.toggleDir('src');
    await settle();
    expect(store.getSnapshot().expanded).toEqual(['src']);
    expect(store.getSnapshot().dirs.src?.listing?.entries).toHaveLength(1);
    expect(listDir).toHaveBeenCalledTimes(2);

    store.toggleDir('src');
    expect(store.getSnapshot().expanded).toEqual([]);
    store.toggleDir('src');
    await settle();

    // The third toggle re-opens the cached listing: two calls for the root and
    // one for src, never a third.
    expect(listDir).toHaveBeenCalledTimes(2);
    expect(store.getSnapshot().dirs.src?.listing?.entries).toHaveLength(1);
  });

  it('keeps a directory error, and retries when the directory is reopened', async () => {
    let attempts = 0;
    const listDir = vi.fn((_sessionId: string, path: string) => {
      attempts += 1;
      if (path === '' && attempts < 2) {
        return Promise.reject(new ApiError('bad_request', 'not inside the workspace', 400));
      }
      return Promise.resolve(path === '' ? listing('', [{ name: 'src', kind: 'dir' }]) : listing(path, []));
    });
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ listDir }) });
    store.start();
    await settle();
    expect(store.getSnapshot().dirs['']?.error).toBe('bad_request: not inside the workspace');

    store.refreshDir('');
    await settle();
    expect(store.getSnapshot().dirs['']?.error).toBeNull();
    expect(store.getSnapshot().dirs['']?.listing?.entries).toEqual([{ name: 'src', kind: 'dir' }]);
  });

  it('opens a file into a tab, reading it once', async () => {
    const readFile = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve(textFile(path, 'export const x = 1;\n')),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ readFile }) });
    store.openFile('src/main.ts');
    await settle();

    const state = store.getSnapshot();
    expect(state.activePath).toBe('src/main.ts');
    expect(state.open).toHaveLength(1);
    expect(state.open[0]?.file?.text).toBe('export const x = 1;\n');
    expect(state.open[0]?.error).toBeNull();

    store.setActive('src/main.ts');
    await settle();
    expect(readFile).toHaveBeenCalledTimes(1);
  });

  it('a failed read leaves a message, never a blank editor', async () => {
    const store = new WorkbenchStore({
      sessionId: SESSION,
      api: fakeApi({
        readFile: () => Promise.reject(new ApiError('bad_request', 'not a text file', 400)),
      }),
    });
    store.openFile('blob.bin');
    await settle();

    const tab = store.getSnapshot().open[0];
    expect(tab?.file).toBeNull();
    expect(tab?.error).toBe('bad_request: not a text file');
  });

  it('closing a tab focuses its neighbour only when it was the active one', async () => {
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi() });
    store.openFile('a.txt');
    store.openFile('b.txt');
    store.openFile('c.txt');
    await settle();
    expect(store.getSnapshot().activePath).toBe('c.txt');

    // Closing a tab that is not in front leaves the focus alone.
    store.closeFile('a.txt');
    expect(store.getSnapshot().activePath).toBe('c.txt');

    // Closing the active one takes the tab that moved into its index (b), and
    // the last one falls back to its left neighbour.
    store.closeFile('c.txt');
    expect(store.getSnapshot().activePath).toBe('b.txt');
    store.closeFile('b.txt');
    expect(store.getSnapshot().activePath).toBeNull();
    expect(store.getSnapshot().open).toEqual([]);
  });

  it('reads the diff once, and only tells empty from unread after answering', async () => {
    const files: DiffEntry[] = [{ path: 'src/a.ts', status: 'modified', additions: 2, deletions: 1 }];
    const listDiff = vi.fn(() => Promise.resolve(files));
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ listDiff }) });

    expect(store.getSnapshot().diff).toBeNull();
    store.toggleDiff();
    await settle();

    expect(store.getSnapshot().showDiff).toBe(true);
    expect(store.getSnapshot().diff).toEqual(files);
    expect(listDiff).toHaveBeenCalledTimes(1);

    store.toggleDiff();
    store.toggleDiff();
    await settle();
    expect(listDiff).toHaveBeenCalledTimes(1);

    // Opening a row leaves the diff view and focuses the file.
    store.toggleDiff();
    store.openFromDiff('src/a.ts');
    await settle();
    expect(store.getSnapshot().showDiff).toBe(false);
    expect(store.getSnapshot().activePath).toBe('src/a.ts');
  });

  it('a diff read that fails keeps its message and retries on request', async () => {
    let attempts = 0;
    const listDiff = vi.fn(() => {
      attempts += 1;
      return attempts === 1
        ? Promise.reject(new ApiError('http_500', 'git unavailable', 500))
        : Promise.resolve([]);
    });
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ listDiff }) });
    store.toggleDiff();
    await settle();
    expect(store.getSnapshot().diff).toBeNull();
    expect(store.getSnapshot().diffError).toBe('http_500: git unavailable');

    store.refreshDiff();
    await settle();
    expect(store.getSnapshot().diffError).toBeNull();
    expect(store.getSnapshot().diff).toEqual([]);
  });
});