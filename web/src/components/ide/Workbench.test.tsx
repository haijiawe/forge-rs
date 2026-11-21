// The workbench as it is drawn: tree, tabs, editor, diff list, and the switch
// that decides between three columns and one.
//
// The fake API answers from a fixed workspace, so every assertion is about what
// the component does with an answer — not about where the answer came from.
// A missing or slow answer is not simulated by a stub returning undefined: the
// store's own tests cover that, and a component test that invents a fourth state
// would prove nothing about the real one.
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../../api/client';
import type { DiffEntry, DirListing, FileText } from '../../api/workbench';
import { interact, mount, waitFor } from '../../test/render';
import { Workbench, WIDE_QUERY, useWideLayout } from './Workbench';
import type { WorkbenchApi } from '../../state/workbench';

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

/** A workspace with one directory and one file at the root. */
function workspace(overrides: Partial<WorkbenchApi> = {}): WorkbenchApi {
  const files: Record<string, FileText> = {
    'readme.md': textFile('readme.md', '# hearth\n'),
    'src/main.ts': textFile('src/main.ts', 'export const x = 1;\n'),
  };
  return {
    listDir: (_sessionId, path) =>
      Promise.resolve(
        path === ''
          ? listing('', [
              { name: 'src', kind: 'dir' },
              { name: 'readme.md', kind: 'file', size: 8 },
            ])
          : path === 'src'
            ? listing('src', [{ name: 'main.ts', kind: 'file', size: 21 }])
            : listing(path, []),
      ),
    readFile: (_sessionId, path) => {
      const file = files[path];
      return file === undefined
        ? Promise.reject(new ApiError('bad_request', `no such file: ${path}`, 400))
        : Promise.resolve(file);
    },
    listDiff: () => Promise.resolve([]),
    readDiff: (_sessionId, path) =>
      Promise.resolve({ path, status: 'modified', text: '', truncated: false }),
    saveFile: (_sessionId, path, text) =>
      Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) }),
    ...overrides,
  };
}

function buttonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const found = Array.from(container.querySelectorAll('button')).find(
    (button) => (button.textContent ?? '').trim() === text,
  );
  if (found === undefined) throw new Error(`no button labelled ${text}`);
  return found;
}

function click(button: HTMLButtonElement): void {
  interact(() => {
    button.click();
  });
}

describe('Workbench', () => {
  it('says the workspace root is empty instead of waiting forever', async () => {
    const api = workspace({ listDir: () => Promise.resolve(listing('', [])) });
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={api}
      />,
    );

    await waitFor(() => (view.container.textContent ?? '').includes('没有列出任何条目'));
    expect(view.container.textContent).toContain('目录确实是空的');
    view.unmount();
  });

  it('expands a directory and then opens a file into a tab', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
      />,
    );

    await waitFor(() => view.container.querySelector('.wb-row-dir[title="src"]') !== null);
    const dir = view.container.querySelector<HTMLButtonElement>('.wb-row-dir[title="src"]');
    if (dir === null) throw new Error('the src row was not drawn');
    click(dir);

    await waitFor(() => (view.container.textContent ?? '').includes('main.ts'));
    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="src/main.ts"]');
    if (file === null) throw new Error('the main.ts row was not drawn');
    click(file);

    await waitFor(() => view.container.querySelector('.wb-textarea') !== null);
    const tab = view.container.querySelector('.wb-tab-label');
    expect(tab?.textContent).toContain('main.ts');
    const editor = view.container.querySelector<HTMLTextAreaElement>('.wb-textarea');
    expect(editor?.value).toBe('export const x = 1;\n');
    // Editable: the server in this fixture was started writable, and a pane that
    // refuses keystrokes without saying why is the bug this replaced.
    expect(editor?.readOnly).toBe(false);

    // The row marks the active file, and the header names its path.
    expect(view.container.querySelector('.wb-row-active')?.getAttribute('title')).toContain('src/main.ts');
    expect(view.container.querySelector('.wb-editor-path')?.textContent).toBe('src/main.ts');
    view.unmount();
  });

  it('shows nothing open before a file is picked, and a tab once one is', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    expect(view.container.textContent).toContain('还没有打开任何文件');
    expect(view.container.querySelector('.wb-textarea')).toBeNull();

    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="readme.md"]');
    if (file === null) throw new Error('the readme row was not drawn');
    click(file);

    await waitFor(() => view.container.querySelector('.wb-textarea') !== null);
    expect(view.container.querySelector('.wb-tab-label')?.textContent).toContain('readme.md');
    expect(view.container.querySelector<HTMLTextAreaElement>('.wb-textarea')?.value).toBe('# hearth\n');
    view.unmount();
  });

  it('marks a typed buffer unsaved and saves it with Ctrl+S', async () => {
    const saveFile = vi.fn((_sessionId: string, path: string, text: string) =>
      Promise.resolve({ path, bytes: text.length, sha256: digestOf(text) }),
    );
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({ saveFile })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="readme.md"]');
    if (file === null) throw new Error('the readme row was not drawn');
    click(file);
    await waitFor(() => view.container.querySelector('.wb-textarea') !== null);

    const editor = view.container.querySelector<HTMLTextAreaElement>('.wb-textarea');
    if (editor === null) throw new Error('no editor was drawn');
    // React's onChange listens for `input`, so the value and the event both have
    // to be set; assigning alone would leave the store believing nothing changed.
    const setValue = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      'value',
    )?.set;
    interact(() => {
      setValue?.call(editor, '# hearth, edited\n');
      editor.dispatchEvent(new Event('input', { bubbles: true }));
    });

    await waitFor(() => (view.container.textContent ?? '').includes('有未保存的改动'));

    interact(() => {
      editor.dispatchEvent(
        new KeyboardEvent('keydown', { key: 's', ctrlKey: true, bubbles: true }),
      );
    });
    await waitFor(() => saveFile.mock.calls.length === 1);
    expect(saveFile).toHaveBeenCalledWith(SESSION, 'readme.md', '# hearth, edited\n', digestOf('# hearth\n'));
    await waitFor(() => !(view.container.textContent ?? '').includes('有未保存的改动'));
    view.unmount();
  });

  it('shows the server sentence when a tab cannot be read', async () => {
    // simulated by a workspace whose file is gone after being listed — the same
    // race a real workspace has when a file is deleted while the tab is open.
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({
          readFile: (_sessionId, path) =>
            Promise.reject(new ApiError('bad_request', `no such file: ${path}`, 400)),
        })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);

    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="readme.md"]');
    if (file === null) throw new Error('the readme row was not drawn');
    click(file);

    await waitFor(() => (view.container.textContent ?? '').includes('no such file: readme.md'));
    expect(view.container.querySelector('.wb-textarea')).toBeNull();
    // The tab survives the failure: dropping it would hide what went wrong.
    expect(view.container.querySelector('.wb-tab-label')?.textContent).toContain('readme.md');
    view.unmount();
  });

  it('opens the diff list from the toolbar and says why it is empty', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);

    click(buttonByText(view.container, '改动'));
    await waitFor(() => (view.container.textContent ?? '').includes('没有改动的文件'));
    expect(view.container.textContent).toContain('工作区不是 git');
    // The editor is replaced, not stacked behind the list.
    expect(view.container.querySelector('.wb-tabs')).toBeNull();
    view.unmount();
  });

  it('lists a changed file with its counts and opens it from the row', async () => {
    const changed: DiffEntry[] = [
      { path: 'src/main.ts', status: 'modified', additions: 3, deletions: 1 },
    ];
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({ listDiff: () => Promise.resolve(changed) })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    click(buttonByText(view.container, '改动'));

    await waitFor(() => view.container.querySelector('.wb-diff-row') !== null);
    expect(view.container.textContent).toContain('+3');
    expect(view.container.textContent).toContain('-1');

    const row = view.container.querySelector<HTMLButtonElement>('.wb-diff-row');
    if (row === null) throw new Error('the diff row was not drawn');
    click(row);

    // Opening a row leaves the diff view: the editor is what the row asked for.
    await waitFor(() => view.container.querySelector('.wb-textarea') !== null);
    expect(view.container.querySelector('.wb-diff')).toBeNull();
    view.unmount();
  });

  it('collapsing the tree drops its column instead of leaving an empty track', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
      />,
    );
    const body = (): HTMLElement | null => view.container.querySelector('.wb-body');
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);

    // Four tracks while the tree is drawn: tree, centre, grip, agent. The
    // string is compared by its first track only — jsdom normalises `minmax(0,…)`
    // to `minmax(0px,…)`, and the point of the assertion is the lead track.
    expect(body()?.style.gridTemplateColumns.startsWith('240px minmax(')).toBe(true);

    click(buttonByText(view.container, '◀ 文件'));
    expect(view.container.querySelector('.wb-tree')).toBeNull();
    // Three, once the tree is gone: a collapsed tree is an absent column, not a
    // 0px track that would swallow the flexible one.
    expect(body()?.style.gridTemplateColumns.startsWith('minmax(')).toBe(true);
    view.unmount();
  });

  it('says when the server truncated a file, and when a file is not text', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({
          readFile: (_sessionId, path) =>
            Promise.resolve(
              path === 'src/main.ts'
                ? {
                    path,
                    size: 2_000_000,
                    text: 'x',
                    truncated: true,
                    binary: false,
                    // No token, deliberately: a truncated read has no digest to
                    // compare, which is exactly the case the server names.
                    sha256: '',
                  }
                : { path, size: 4, text: '', truncated: false, binary: true, sha256: '' },
            ),
        })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-dir[title="src"]') !== null);
    const dir = view.container.querySelector<HTMLButtonElement>('.wb-row-dir[title="src"]');
    if (dir === null) throw new Error('the src row was not drawn');
    click(dir);
    await waitFor(() => view.container.querySelector('.wb-row-file[title^="src/main.ts"]') !== null);
    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="src/main.ts"]');
    if (file === null) throw new Error('the main.ts row was not drawn');
    click(file);

    await waitFor(() => view.container.querySelector('.wb-editor-notice') !== null);
    expect(view.container.querySelector('.wb-editor-notice')?.textContent).toContain('截断');

    // A binary file's notice replaces nothing: the same pane says there is no
    // text, because showing an empty box would look like an empty file.
    const readme = view.container.querySelector<HTMLButtonElement>('.wb-row-file[title^="readme.md"]');
    if (readme === null) throw new Error('the readme row was not drawn');
    click(readme);
    await waitFor(() =>
      (view.container.querySelector('.wb-editor-notice')?.textContent ?? '').includes('不是文本'),
    );
    view.unmount();
  });

  it('keeps a failed diff read visible, with a retry', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({
          listDiff: () => Promise.reject(new ApiError('http_500', 'git unavailable', 500)),
        })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    click(buttonByText(view.container, '改动'));

    await waitFor(() => (view.container.textContent ?? '').includes('git unavailable'));
    expect(buttonByText(view.container, '重试')).toBeDefined();
    view.unmount();
  });

  it('expands a row into its lines and colours them by first character', async () => {
    const changed: DiffEntry[] = [
      { path: 'src/main.ts', status: 'modified', additions: 1, deletions: 1 },
    ];
    const diffText = '@@ -1 +1 @@\n-export const x = 1;\n+export const x = 2;\n';
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace({
          listDiff: () => Promise.resolve(changed),
          readDiff: () => Promise.resolve({ path: 'src/main.ts', status: 'modified', text: diffText, truncated: false }),
        })}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    click(buttonByText(view.container, '改动'));

    await waitFor(() => view.container.querySelector('.wb-diff-toggle') !== null);
    const toggle = view.container.querySelector<HTMLButtonElement>('.wb-diff-toggle');
    if (toggle === null) throw new Error('the diff row has no caret');
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    click(toggle);

    await waitFor(() => view.container.querySelector('.wb-diff-line') !== null);
    // The row stays on screen: judging a change must not cost the list.
    expect(view.container.querySelector('.wb-diff-row')).not.toBeNull();
    expect(view.container.querySelector('.wb-diff-toggle')?.getAttribute('aria-expanded')).toBe('true');

    // Every line is drawn, and each is classified by its own first character
    // rather than by position, so a hunk that starts with `-` stays a deletion.
    const classes = Array.from(view.container.querySelectorAll('.wb-diff-line')).map(
      (line) => line.className,
    );
    expect(classes.some((name) => name.includes('wb-diff-hunk'))).toBe(true);
    expect(classes.some((name) => name.includes('wb-diff-del'))).toBe(true);
    expect(classes.some((name) => name.includes('wb-diff-add'))).toBe(true);
    expect(view.container.textContent).toContain('export const x = 2;');
    view.unmount();
  });

  it('asks before a tab with unsaved changes is closed', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-row-file') !== null);
    const file = view.container.querySelector<HTMLButtonElement>('.wb-row-file');
    if (file === null) throw new Error('no file row');
    click(file);
    await waitFor(() => view.container.querySelector('.wb-textarea') !== null);

    const editor = view.container.querySelector<HTMLTextAreaElement>('.wb-textarea');
    if (editor === null) throw new Error('no editor');
    const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
    await interact(() => {
      setValue?.call(editor, '# edited\n');
      editor.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await waitFor(() => (view.container.textContent ?? '').includes('未保存'));

    const close = view.container.querySelector<HTMLButtonElement>('.wb-tab-close');
    if (close === null) throw new Error('no close button');
    await interact(() => {
      click(close);
    });

    // The question is inline and names the file, and the tab is still there.
    await waitFor(() => view.container.querySelector('.wb-confirm') !== null);
    expect(view.container.textContent).toContain('有未保存的改动');
    expect(view.container.querySelector('.wb-tab')).not.toBeNull();

    await interact(() => {
      click(buttonByText(view.container, '继续编辑'));
    });
    expect(view.container.querySelector('.wb-confirm')).toBeNull();
    expect(view.container.querySelector('.wb-tab')).not.toBeNull();
    view.unmount();
  });
});

describe('useWideLayout', () => {
  function Probe() {
    return <span>{useWideLayout() ? 'wide' : 'narrow'}</span>;
  }

  it('answers narrow when the host has no matchMedia', () => {
    // jsdom has no window.matchMedia, which is exactly the case the fallback
    // exists for: an unknown viewport gets the screen that was tested.
    const view = mount(<Probe />);
    expect(view.container.textContent).toBe('narrow');
    view.unmount();
  });

  it('follows a matchMedia answer when the host has one', () => {
    const listeners = new Set<() => void>();
    const stub = vi.fn((query: string) => ({
      matches: query === WIDE_QUERY,
      media: query,
      addEventListener: (_event: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_event: string, listener: () => void) => listeners.delete(listener),
    }));
    vi.stubGlobal('matchMedia', stub);
    try {
      const view = mount(<Probe />);
      expect(stub).toHaveBeenCalledWith(WIDE_QUERY);
      expect(view.container.textContent).toBe('wide');
      view.unmount();
      expect(listeners.size).toBe(0);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});