// 搜索面板的纯组件测试：给 props，看它画了什么、点了回调什么。
//
// 搜索的**状态机**（发起、晚到的答案被丢掉、reveal 只给记录它的文件）由
// `state/workbench.test.ts` 守着；这里只管渲染与手势。两者分开是因为把组件
// 测试写成一个异步管线，会让「画错了」和「等错了」混在同一个断言里。
import { describe, expect, it, vi } from 'vitest';

import { interact, mount } from '../../test/render';
import type { SearchState } from '../../state/workbench';
import type { SearchAnswer } from '../../api/workbench';
import { SearchPanel } from './SearchPanel';

function answer(over: Partial<SearchAnswer> = {}): SearchAnswer {
  return { query: 'needle', files: 12, truncated: false, hits: [], ...over };
}

function state(over: Partial<SearchState> = {}): SearchState {
  return { query: 'needle', loading: false, error: null, answer: answer(), ...over };
}

function props(search: SearchState | null) {
  return {
    search,
    onSearch: vi.fn(),
    onClear: vi.fn(),
    onOpenHit: vi.fn(),
  };
}

describe('SearchPanel', () => {
  it('画出一条命中的路径、行号与那一行文字', () => {
    const handlers = props(
      state({
        answer: answer({
          hits: [{ path: 'src/deep/thing.rs', line: 2, column: 9, text: 'let 名字 = 1;' }],
        }),
      }),
    );
    const view = mount(<SearchPanel {...handlers} />);
    const hits = view.container.querySelectorAll('.wb-search-hit');
    expect(hits.length).toBe(1);
    expect(hits[0]?.querySelector('.wb-search-where')?.textContent).toBe('src/deep/thing.rs:2');
    expect(hits[0]?.querySelector('.wb-search-text')?.textContent).toBe('let 名字 = 1;');
    expect(view.container.querySelector('.wb-search-count')?.textContent).toContain(
      '1 条匹配，扫过 12 个文件',
    );
    view.unmount();
  });

  it('点一条命中就报出它的路径与行号', async () => {
    const handlers = props(
      state({
        answer: answer({
          hits: [{ path: 'src/main.rs', line: 7, column: 3, text: 'fn main() {}' }],
        }),
      }),
    );
    const view = mount(<SearchPanel {...handlers} />);
    const hit = view.container.querySelector<HTMLButtonElement>('.wb-search-hit');
    await interact(() => {
      hit?.click();
    });
    expect(handlers.onOpenHit).toHaveBeenCalledWith('src/main.rs', 7);
    view.unmount();
  });

  it('没有命中、而且搜索提前停下时，「没有匹配」与「结果不完整」都说得出来', () => {
    const handlers = props(
      state({ answer: answer({ files: 3, truncated: true, hits: [] }) }),
    );
    const view = mount(<SearchPanel {...handlers} />);
    const text = view.container.textContent ?? '';
    expect(text).toContain('没有匹配');
    expect(text).toContain('扫过 3 个文件');
    expect(text).toContain('结果不完整');
    view.unmount();
  });

  it('没有命中但确实扫完了，就不说结果不完整', () => {
    const handlers = props(state({ answer: answer({ files: 3, truncated: false, hits: [] }) }));
    const view = mount(<SearchPanel {...handlers} />);
    expect(view.container.textContent ?? '').not.toContain('结果不完整');
    view.unmount();
  });

  it('空着提交是清除，不是搜一个空串', async () => {
    const handlers = props(state());
    const view = mount(<SearchPanel {...handlers} />);
    const form = view.container.querySelector<HTMLFormElement>('form.wb-search-form');
    await interact(() => {
      form?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    });
    expect(handlers.onClear).toHaveBeenCalledTimes(1);
    expect(handlers.onSearch).not.toHaveBeenCalled();
    view.unmount();
  });

  it('加载时说正在搜索，失败时把服务端的话原样摆出来', () => {
    const busy = props(state({ loading: true, answer: null }));
    const view = mount(<SearchPanel {...busy} />);
    expect(view.container.textContent ?? '').toContain('正在搜索');
    view.unmount();

    const failed = props(state({ error: '搜索失败了：磁盘出错', answer: null }));
    const second = mount(<SearchPanel {...failed} />);
    expect(second.container.textContent ?? '').toContain('磁盘出错');
    second.unmount();
  });
});