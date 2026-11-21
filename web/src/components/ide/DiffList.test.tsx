import { describe, expect, it } from 'vitest';
import { DiffList } from './DiffList';
import type { DiffEntry, FileDiff } from '../../api/workbench';
import type { FileDiffState } from '../../state/workbench';
import { interact, mount } from '../../test/render';

const file: DiffEntry = { path: 'src/a.rs', status: 'modified', additions: 2, deletions: 1 };

function expanded(body: FileDiff): FileDiffState {
  return { path: body.path, loading: false, error: null, body };
}

function renderWith(body: FileDiff | null, side = false) {
  const view = mount(
    <DiffList
      files={[file]}
      loading={false}
      error={null}
      fileDiff={body === null ? null : expanded(body)}
      onOpenFile={() => undefined}
      onRetry={() => undefined}
      onToggleLines={() => undefined}
      onHideLines={() => undefined}
      onRetryLines={() => undefined}
      onRevert={() => undefined}
      revertingPath={null}
      revertError={null}
    />,
  );
  if (side) {
    const toggle = view.container.querySelector('.wb-diff-toggle');
    interact(() => {
      (toggle as HTMLButtonElement).click();
    });
    const sideButton = Array.from(view.container.querySelectorAll('button')).find(
      (b) => b.textContent === '并排显示',
    );
    interact(() => {
      (sideButton as HTMLButtonElement).click();
    });
  } else {
    const toggle = view.container.querySelector('.wb-diff-toggle');
    interact(() => {
      (toggle as HTMLButtonElement).click();
    });
  }
  return view;
}

const BODY: FileDiff = {
  path: 'src/a.rs',
  status: 'modified',
  // A文件头带一个空格，内容行不带：这正是判据容易写错的地方。
  text: [
    'diff --git a/src/a.rs b/src/a.rs',
    '--- a/src/a.rs',
    '+++ b/src/a.rs',
    '@@ -1,2 +1,2 @@',
    ' let a = 1;',
    '---baz',
    '+++bar',
    '',
  ].join('\n'),
  truncated: false,
};

describe('DiffList — 统一格式的行色', () => {
  it('treats "+++"/"---" as a file header only when a space follows', () => {
    const view = renderWith(BODY);
    const lines = Array.from(view.container.querySelectorAll('.wb-diff-line'));
    const classes = lines.map((line) => line.className);
    // 前三行是 git 的元信息（diff / --- a / +++ b）。
    expect(classes[0]).toContain('wb-diff-meta');
    expect(classes[1]).toContain('wb-diff-meta');
    expect(classes[2]).toContain('wb-diff-meta');
    // "---baz" 是删掉的一行，它的内容以 "--baz" 开头；"+++bar" 是新增的一行，
    // 内容以 "++bar" 开头。它们看起来像文件头，其实是内容。
    const del = lines.find((line) => line.textContent?.includes('---baz'));
    const add = lines.find((line) => line.textContent?.includes('+++bar'));
    expect(del?.className).toContain('wb-diff-del');
    expect(add?.className).toContain('wb-diff-add');
    view.unmount();
  });
});

describe('DiffList — 并排', () => {
  it('draws a table whose per-side rows have two text cells each', () => {
    const view = renderWith(BODY, true);
    const table = view.container.querySelector('table.wb-side');
    expect(table).not.toBeNull();
    const rows = Array.from(view.container.querySelectorAll('.wb-side tbody tr'));
    expect(rows.length).toBeGreaterThan(0);
    let wide = 0;
    let paired = 0;
    for (const row of rows) {
      const cells = row.querySelectorAll('td');
      if (cells.length === 1) {
        // 没有对应行的增减行横跨两列（JSX 里的 colSpan={4}）：一行被拆成两行时，
        // 并排视图本来就没有正确答案，留白比假装成对更诚实。
        expect(cells[0]?.getAttribute('colspan')).toBe('4');
        wide += 1;
      } else {
        expect(cells.length).toBe(4);
        paired += 1;
      }
    }
    // hunk 头与文件头是 wide 行，"---baz"/"+++bar" 是配对行。
    expect(wide).toBeGreaterThan(0);
    expect(paired).toBeGreaterThan(0);
    view.unmount();
  });
});
