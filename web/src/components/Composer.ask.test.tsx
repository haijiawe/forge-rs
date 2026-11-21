// 输入框这一侧：编辑器交来的选区要变成草稿，追加而不是替换，光标落在末尾。
//
// 追加和光标位置都不是外观问题：用户常常先写一半问题再去选代码，替换会把
// 那半句吞掉；光标不在末尾的话，下一条消息第一句会插进代码块中间。
import { describe, expect, it } from 'vitest';
import { Composer } from './Composer';
import { interact, mount, typeInto, waitFor } from '../test/render';
import { askAboutSelection } from '../state/ask';

const accept = async (): Promise<boolean> => true;

function areaOf(container: HTMLElement): HTMLTextAreaElement {
  const element = container.querySelector('textarea');
  if (element === null) throw new Error('no textarea');
  return element;
}

describe('Composer · 编辑器选区', () => {
  it('turns the selection into a quoted block with its file and lines', () => {
    const view = mount(<Composer disabled={false} onSend={accept} />);
    const area = areaOf(view.container);
    interact(() => {
      askAboutSelection({ path: 'src/main.rs', fromLine: 2, toLine: 3, text: 'let a = 1;' });
    });
    expect(area.value).toContain('文件 src/main.rs，第 2–3 行');
    expect(area.value).toContain('let a = 1;');
    // 空行收尾是给问题留的位置：引用是前缀，不是消息的全部。
    expect(area.value.endsWith('\n\n')).toBe(true);
    view.unmount();
  });

  it('appends to a draft the user already started', () => {
    const view = mount(<Composer disabled={false} onSend={accept} />);
    const area = areaOf(view.container);
    typeInto(area, '这个函数为什么慢？');
    interact(() => {
      askAboutSelection({ path: 'src/slow.rs', fromLine: 9, toLine: 11, text: 'loop {}' });
    });
    expect(area.value.startsWith('这个函数为什么慢？')).toBe(true);
    expect(area.value).toContain('文件 src/slow.rs，第 9–11 行');
    view.unmount();
  });

  it('leaves the caret at the end, after the quote', async () => {
    const view = mount(<Composer disabled={false} onSend={accept} />);
    const area = areaOf(view.container);
    interact(() => {
      askAboutSelection({ path: 'a.ts', fromLine: 1, toLine: 1, text: 'x' });
    });
    await waitFor(() => document.activeElement === area);
    expect(area.selectionStart).toBe(area.value.length);
    expect(area.selectionEnd).toBe(area.value.length);
    view.unmount();
  });
});
