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
  return { path, size: text.length, text, truncated: false, binary: false, sha256: digestOf(text) };
}

/** A stand-in digest: the tests care that a token round-trips, not what it is. */
function digestOf(text: string): string {
  let hash = 0;
  for (let index = 0; index < text.length; index += 1) {
    hash = (hash * 31 + text.charCodeAt(index)) >>> 0;
  }
  return String(hash).padStart(8, '0').repeat(8).slice(0, 64);
}

function fakeApi(overrides: Partial<WorkbenchApi> = {}): WorkbenchApi {
  return {
    listDir: () => Promise.resolve(listing('', [])),
    readFile: (_sessionId, path) => Promise.resolve(textFile(path, '')),
    listDiff: () => Promise.resolve([]),
    readDiff: (_sessionId, path) =>
      Promise.resolve({ path, status: 'modified', text: '', truncated: false }),
    saveFile: (_sessionId, path, text) =>
      Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) }),
    revertFile: (_sessionId, path) =>
      Promise.resolve({ path, bytes: 0, sha256: digestOf('') }),
    searchFiles: (_sessionId, query) =>
      Promise.resolve({ query, files: 0, truncated: false, hits: [] }),
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

  /** Start the store on an empty root and leave it settled; no file is opened. */
  async function opened(api: WorkbenchApi): Promise<WorkbenchStore> {
    const store = new WorkbenchStore({ sessionId: SESSION, api });
    store.start();
    await settle();
    return store;
  }

  /** Open one file and leave the store settled with that tab active. */
  async function openOne(api: WorkbenchApi): Promise<WorkbenchStore> {
    const store = new WorkbenchStore({ sessionId: SESSION, api });
    store.start();
    await settle();
    store.openFile('a.ts');
    await settle();
    return store;
  }

  it('keeps an edit as a draft and sends the digest it was built from', async () => {
    const original = 'let x = 1;\n';
    const saveFile = vi.fn((_sessionId: string, path: string, text: string) =>
      Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) }),
    );
    const api = fakeApi({
      readFile: (_sessionId, path) => Promise.resolve(textFile(path, original)),
      saveFile,
    });
    const store = await openOne(api);

    store.editFile('a.ts', 'let x = 2;\n');
    expect(store.getSnapshot().open[0]?.draft).toBe('let x = 2;\n');

    await store.saveFile('a.ts');
    await settle();

    // The token is what makes a stale save detectable, so it must be the digest
    // of the bytes that were read — not of the bytes being written.
    expect(saveFile).toHaveBeenCalledWith(SESSION, 'a.ts', 'let x = 2;\n', digestOf(original));
    const tab = store.getSnapshot().open[0];
    expect(tab?.draft).toBeNull();
    expect(tab?.saveError).toBeNull();
    // The tab adopts the answer, which is what lets a second save work.
    expect(tab?.file?.text).toBe('let x = 2;\n');
    expect(tab?.file?.sha256).toBe(digestOf('let x = 2;\n'));
  });

  it('a save that lands on a file that moved keeps the draft and says conflict', async () => {
    const api = fakeApi({
      readFile: (_sessionId, path) => Promise.resolve(textFile(path, 'one\n')),
      saveFile: () =>
        Promise.reject(
          new ApiError('conflict', 'the file changed since it was opened; reload it before saving', 409),
        ),
    });
    const store = await openOne(api);
    store.editFile('a.ts', 'two\n');

    await store.saveFile('a.ts');
    await settle();

    const tab = store.getSnapshot().open[0];
    // The work is still here: a refused save that discarded the buffer would
    // throw away the only copy of what the operator typed.
    expect(tab?.draft).toBe('two\n');
    expect(tab?.saveError).toBe(
      'conflict: the file changed since it was opened; reload it before saving',
    );
    // And the file as read is unchanged: nothing was adopted from a refusal.
    expect(tab?.file?.text).toBe('one\n');
  });

  it('typing back to the bytes that were read is clean, not an unsaved change', async () => {
    const store = await openOne(
      fakeApi({ readFile: (_sessionId, path) => Promise.resolve(textFile(path, 'same\n')) }),
    );
    store.editFile('a.ts', 'other\n');
    expect(store.getSnapshot().open[0]?.draft).toBe('other\n');
    store.editFile('a.ts', 'same\n');
    expect(store.getSnapshot().open[0]?.draft).toBeNull();
  });

  it('saving a clean tab sends nothing at all', async () => {
    const saveFile = vi.fn();
    const store = await openOne(
      fakeApi({
        readFile: (_sessionId, path) => Promise.resolve(textFile(path, 'x\n')),
        saveFile: (sessionId, path, text) => {
          saveFile(sessionId, path, text);
          return Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) });
        },
      }),
    );
    await store.saveFile('a.ts');
    await settle();
    expect(saveFile).not.toHaveBeenCalled();
  });

  it('expands one diff row and collapses it again', async () => {
    const readDiff = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve({
        path,
        status: 'modified',
        text: '@@ -1 +1 @@\n-old\n+new\n',
        truncated: false,
      }),
    );
    const store = await opened(fakeApi({ readDiff }));

    store.toggleFileDiff('a.ts');
    await settle();

    // Loading is published before the answer, so the row draws a spinner rather
    // than an empty frame while the request is in flight.
    expect(store.getSnapshot().fileDiff?.path).toBe('a.ts');
    expect(store.getSnapshot().fileDiff?.body?.text).toContain('+new');
    expect(readDiff).toHaveBeenCalledWith(SESSION, 'a.ts');

    store.toggleFileDiff('a.ts');
    expect(store.getSnapshot().fileDiff).toBeNull();

    // Collapsing did not forget the answer: re-expanding reads again, because a
    // cached diff would be a claim about the disk nobody re-checked.
    store.toggleFileDiff('a.ts');
    await settle();
    expect(readDiff).toHaveBeenCalledTimes(2);
  });

  it('drops a diff answer for a row that is no longer open', async () => {
    // The resolver is parked in an array rather than a `let`: TypeScript narrows
    // a `let` assigned only inside a callback to its initial `null`, and an
    // index read is not narrowed, so this stays callable at the call site.
    const pending: ((text: string) => void)[] = [];
    const store = await opened(
      fakeApi({
        readDiff: (_sessionId, path) =>
          new Promise((resolve) => {
            pending.push((text) => {
              resolve({ path, status: 'modified', text, truncated: false });
            });
          }),
      }),
    );

    store.toggleFileDiff('a.ts');
    await settle();
    store.hideFileDiff();

    // The read was still in flight when the row was collapsed. Its answer must
    // not resurrect the row, and must not appear under whatever is open now.
    pending[0]?.('+late\n');
    await settle();
    expect(store.getSnapshot().fileDiff).toBeNull();
  });

  it('keeps a diff failure on the row instead of blanking the pane', async () => {
    const store = await opened(
      fakeApi({
        readDiff: () => Promise.reject(new ApiError('bad_request', 'git unavailable', 400)),
      }),
    );

    store.toggleFileDiff('a.ts');
    await settle();

    const state = store.getSnapshot().fileDiff;
    expect(state?.error).toBe('bad_request: git unavailable');
    expect(state?.body).toBeNull();
    // The row is still expanded: collapsing it would hide the error behind a
    // caret the operator has no reason to click twice.
    expect(state?.path).toBe('a.ts');
  });

  it('asks before closing a tab with unsaved changes, and closes on confirm', async () => {
    const store = await openOne(fakeApi());
    store.editFile('a.ts', 'edited\n');

    store.closeFile('a.ts');
    // Parked, not closed: the tab and its buffer are exactly as they were.
    expect(store.getSnapshot().pendingClose).toBe('a.ts');
    expect(store.getSnapshot().open.map((tab) => tab.path)).toEqual(['a.ts']);
    expect(store.getSnapshot().open[0]?.draft).toBe('edited\n');

    store.confirmClose();
    expect(store.getSnapshot().pendingClose).toBeNull();
    expect(store.getSnapshot().open).toEqual([]);
  });

  it('cancel keeps the draft and the question goes away', async () => {
    const store = await openOne(fakeApi());
    store.editFile('a.ts', 'edited\n');
    store.closeFile('a.ts');
    store.cancelClose();

    expect(store.getSnapshot().pendingClose).toBeNull();
    expect(store.getSnapshot().open.map((tab) => tab.path)).toEqual(['a.ts']);
    expect(store.getSnapshot().open[0]?.draft).toBe('edited\n');
  });

  it('closes a clean tab without asking', async () => {
    const store = await openOne(fakeApi());
    store.closeFile('a.ts');

    // No question: a confirmation on work that cannot be lost trains the
    // operator to click through the one that matters.
    expect(store.getSnapshot().pendingClose).toBeNull();
    expect(store.getSnapshot().open).toEqual([]);
  });

  it('a pending close does not outlive the tab it was about', async () => {
    const store = await openOne(fakeApi());
    store.editFile('a.ts', 'edited\n');
    store.closeFile('a.ts');
    store.confirmClose();

    // Closing through the confirmation clears both the tab and the question;
    // a stale `pendingClose` would leave the pane asking about nothing.
    expect(store.getSnapshot().pendingClose).toBeNull();
    expect(store.getSnapshot().open).toEqual([]);
  });

  it('saves every dirty tab, one after another, and keeps going past a failure', async () => {
    const order: string[] = [];
    const saveFile = vi.fn((_sessionId: string, path: string, text: string) => {
      order.push(path);
      if (path === 'a.ts') {
        return Promise.reject(new ApiError('conflict', 'the file changed since it was opened', 409));
      }
      return Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) });
    });
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ saveFile }) });
    store.openFile('a.ts');
    store.openFile('b.ts');
    await settle();
    store.editFile('a.ts', 'one\n');
    store.editFile('b.ts', 'two\n');

    await store.saveAll();

    // Sequential, not parallel: two saves racing through one server put their
    // `file/write` lines in an order nobody chose.
    expect(order).toEqual(['a.ts', 'b.ts']);
    // The failure on the first tab did not stop the second, and each tab holds
    // its own verdict.
    const tabs = store.getSnapshot().open;
    expect(tabs.find((tab) => tab.path === 'a.ts')?.saveError).toContain('conflict');
    expect(tabs.find((tab) => tab.path === 'b.ts')?.draft).toBeNull();
    expect(tabs.find((tab) => tab.path === 'b.ts')?.file?.text).toBe('two\n');
  });

  it('saves nothing when every tab is clean', async () => {
    const saveFile = vi.fn(() => Promise.resolve({ path: 'a.ts', bytes: 0, sha256: digestOf('') }));
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ saveFile }) });
    store.openFile('a.ts');
    await settle();

    await store.saveAll();
    expect(saveFile).not.toHaveBeenCalled();
  });

  it('restores the committed bytes and adopts the digest the server reported', async () => {
    const revertFile = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve({ path, bytes: 3, sha256: digestOf('old') }),
    );
    const readFile = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve(textFile(path, 'old')),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ revertFile, readFile }) });
    store.openFile('a.ts');
    await settle();
    store.editFile('a.ts', 'changed\n');
    // The draft is thrown away first, deliberately: restore is not discard.
    store.discardDraft('a.ts');

    const refusal = await store.revertToCommitted('a.ts');

    expect(refusal).toBeNull();
    const tab = store.getSnapshot().open[0];
    // The text comes from the re-read, and the digest from the restore: the tab
    // must be able to save again without a manual reload.
    expect(tab?.file?.text).toBe('old');
    expect(tab?.file?.sha256).toBe(digestOf('old'));
    expect(tab?.draft).toBeNull();
  });

  it('refuses to restore over a draft and says why instead of throwing it away', async () => {
    const revertFile = vi.fn((_sessionId: string, path: string) =>
      Promise.resolve({ path, bytes: 0, sha256: digestOf('') }),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ revertFile }) });
    store.openFile('a.ts');
    await settle();
    store.editFile('a.ts', 'unsaved\n');

    const refusal = await store.revertToCommitted('a.ts');

    // Nothing was written and the draft survives: the button said "restore",
    // not "discard", so it must not discard.
    expect(revertFile).not.toHaveBeenCalled();
    expect(refusal).toContain('未保存');
    expect(store.getSnapshot().open[0]?.draft).toBe('unsaved\n');
  });

  it('reports a refused restore instead of only parking it on a tab that may be closed', async () => {
    const revertFile = vi.fn(() =>
      Promise.reject(new ApiError('conflict', 'git has no committed version of that file', 409)),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ revertFile }) });

    const refusal = await store.revertToCommitted('notes.txt');

    expect(refusal).toContain('no committed version');
  });

  it('searches once per submit and keeps the answer while a hit is opened', async () => {
    const searchFiles = vi.fn((_sessionId: string, query: string) =>
      Promise.resolve({
        query,
        files: 12,
        truncated: false,
        hits: [{ path: 'src/main.ts', line: 7, column: 3, text: 'const needle = 1;' }],
      }),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ searchFiles }) });
    store.start();
    await settle();

    store.search('needle');
    expect(store.getSnapshot().search?.loading).toBe(true);
    await settle();
    expect(searchFiles).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot().search?.answer?.hits).toHaveLength(1);

    // 打开命中结果不该把结果列表扔掉：读者通常是在往下数第十二条。
    store.openHit('src/main.ts', 7);
    await settle();
    expect(store.getSnapshot().search?.answer?.hits).toHaveLength(1);
    expect(store.getSnapshot().activePath).toBe('src/main.ts');
    expect(store.getSnapshot().reveal).toEqual({ path: 'src/main.ts', line: 7 });
  });

  it('clears the results when the box is emptied rather than searching for nothing', async () => {
    const searchFiles = vi.fn((_sessionId: string, query: string) =>
      Promise.resolve({ query, files: 1, truncated: false, hits: [] }),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ searchFiles }) });
    store.start();
    await settle();

    store.search('a');
    await settle();
    expect(store.getSnapshot().search).not.toBeNull();

    store.search('   ');
    await settle();
    // 空查询服务端会拒（它会匹配每一行）；这里连请求都不该发出去。
    expect(searchFiles).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot().search).toBeNull();
  });

  it('drops a search answer that arrived after the box moved on', async () => {
    const pending: ((answer: { query: string; files: number; truncated: boolean; hits: never[] }) => void)[] = [];
    const searchFiles = vi.fn(
      (_sessionId: string, query: string) =>
        new Promise<{ query: string; files: number; truncated: boolean; hits: never[] }>((resolve) => {
          pending.push((answer) => {
            resolve({ ...answer, query });
          });
        }),
    );
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi({ searchFiles }) });
    store.start();
    await settle();

    store.search('first');
    store.search('second');
    // 第一个查询的答案晚到：它已经不对应输入框里的内容了，留着就是「看起来像答案」的错误。
    pending[0]?.({ query: 'first', files: 3, truncated: false, hits: [] });
    await settle();
    expect(store.getSnapshot().search?.answer).toBeNull();
    expect(store.getSnapshot().search?.query).toBe('second');

    pending[1]?.({ query: 'second', files: 4, truncated: false, hits: [] });
    await settle();
    expect(store.getSnapshot().search?.answer?.files).toBe(4);
  });

  it('keeps a reveal only for the file it was recorded for', async () => {
    const store = new WorkbenchStore({ sessionId: SESSION, api: fakeApi() });
    store.openFile('a.ts');
    await settle();

    store.openHit('a.ts', 3);
    expect(store.getSnapshot().reveal).toEqual({ path: 'a.ts', line: 3 });

    store.clearReveal();
    expect(store.getSnapshot().reveal).toBeNull();
  });
});