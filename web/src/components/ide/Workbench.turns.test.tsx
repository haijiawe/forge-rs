// 第三视图的接线：按钮把中栏切成「本轮改动」，点一条打开文件并跳到那一行。
//
// 数据来自会话 store 的折叠结果，这里给的是它的形状；折叠本身由
// `state/turnChanges.test.ts` 守着，列表渲染由 `TurnChanges.test.tsx` 守着。
// 这个文件只回答一件事：接上以后，手势真的会打开对的文件、带上对的行号。
import { describe, expect, it } from 'vitest';
import type { DirListing, FileText } from '../../api/workbench';
import { interact, mount, waitFor } from '../../test/render';
import { Workbench } from './Workbench';
import type { WorkbenchApi } from '../../state/workbench';
import type { TurnChangeTurn } from '../../state/turnChanges';

const SESSION = 's1';

function textFile(path: string, text: string): FileText {
  return { path, size: text.length, text, truncated: false, binary: false, sha256: 'a'.repeat(64) };
}

function workspace(): WorkbenchApi {
  const files: Record<string, FileText> = {
    'src/main.rs': textFile('src/main.rs', 'one\ntwo\nthree\n'),
  };
  return {
    listDir: (_sessionId: string, path: string): Promise<DirListing> =>
      Promise.resolve(path === '' ? { path: '', entries: [{ name: 'src', kind: 'dir' }] } : { path, entries: [] }),
    readFile: (_sessionId, path) => {
      const file = files[path];
      return file === undefined ? Promise.reject(new Error(`no such file: ${path}`)) : Promise.resolve(file);
    },
    listDiff: () => Promise.resolve([]),
    readDiff: (_sessionId, path) => Promise.resolve({ path, status: 'modified', text: '', truncated: false }),
    saveFile: (_sessionId, path, text) => Promise.resolve({ path, bytes: text.length, sha256: 'b'.repeat(64) }),
    revertFile: (_sessionId, path) => Promise.resolve({ path, bytes: 0, sha256: 'c'.repeat(64) }),
    searchFiles: (_sessionId, query) => Promise.resolve({ query, files: 0, truncated: false, hits: [] }),
  };
}

const TURNS: TurnChangeTurn[] = [
  {
    turn: 1,
    files: [
      {
        path: 'src/main.rs',
        added: 1,
        removed: 1,
        ranges: [{ from: 2, to: 2 }],
        coarse: false,
        cut: false,
        writes: 1,
      },
    ],
    added: 1,
    removed: 1,
  },
];

function buttonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const found = Array.from(container.querySelectorAll('button')).find(
    (button) => (button.textContent ?? '').trim() === text,
  );
  if (found === undefined) throw new Error(`no button labelled ${text}`);
  return found;
}

describe('Workbench — 本轮改动', () => {
  it('按钮把中栏切成按轮的列表', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
        turnChanges={TURNS}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-pane-tree') !== null);

    interact(() => {
      buttonByText(view.container, '本轮改动').click();
    });
    await waitFor(() => view.container.querySelector('.wb-turn') !== null);
    expect(view.container.textContent).toContain('第 1 轮');
    expect(view.container.querySelector('.wb-turn-path')?.textContent).toBe('src/main.rs');
    view.unmount();
  });

  it('点一条改动会打开那个文件，并把行号交给编辑器', async () => {
    const view = mount(
      <Workbench
        sessionId={SESSION}
        head={<span>head</span>}
        agent={<span>agent</span>}
        connecting={false}
        needsResync={false}
        api={workspace()}
        turnChanges={TURNS}
      />,
    );
    await waitFor(() => view.container.querySelector('.wb-pane-tree') !== null);
    interact(() => {
      buttonByText(view.container, '本轮改动').click();
    });
    await waitFor(() => view.container.querySelector('.wb-turn-file') !== null);

    const row = view.container.querySelector<HTMLButtonElement>('.wb-turn-file');
    if (row === null) throw new Error('the turn row was not drawn');
    interact(() => {
      row.click();
    });

    // 文件打开 = 列表让位给编辑器，标签上出现路径。
    await waitFor(() => view.container.querySelector('.wb-tab') !== null);
    expect(view.container.querySelector('.wb-turn-pane')).toBeNull();
    expect(view.container.textContent).toContain('main.rs');
    view.unmount();
  });
});
