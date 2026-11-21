import { describe, expect, it } from 'vitest';
import { COLLAPSED_LINES, argHint, collapseOutput } from './ToolCard';
import { ToolCard } from './ToolCard';
import type { ToolRow } from '../state/rows';
import { interact, mount } from '../test/render';

function toolRow(overrides: Partial<ToolRow> = {}): ToolRow {
  return {
    kind: 'tool',
    key: 'tool:call-1',
    seq: 7,
    ts: 1_700_000_000_000,
    callId: 'call-1',
    tool: 'shell',
    args: { cmd: 'cat big.log' },
    truncated: false,
    done: true,
    ...overrides,
  };
}

describe('collapseOutput', () => {
  it('leaves a short output untouched', () => {
    const result = collapseOutput('one\ntwo\nthree');
    expect(result.collapsed).toBe(false);
    expect(result.omitted).toBe(0);
    expect(result.lines).toBe(3);
    expect(result.text).toBe('one\ntwo\nthree');
  });

  it('keeps the first and last 20 lines of a long output', () => {
    const lines = Array.from({ length: 100 }, (_, index) => `line-${String(index + 1)}`);
    const result = collapseOutput(lines.join('\n'));
    expect(result.lines).toBe(100);
    expect(result.collapsed).toBe(true);
    expect(result.omitted).toBe(100 - COLLAPSED_LINES * 2);
    expect(result.text).toContain('line-1\n');
    expect(result.text).toContain('… 已隐藏 60 行 …');
    expect(result.text).toContain('line-100');
    // The middle is gone, not just visually hidden.
    expect(result.text).not.toContain('line-50\n');
  });
});

describe('ToolCard', () => {
  it('renders the truncation marker the server set', () => {
    const view = mount(<ToolCard row={toolRow({ output: 'a\nb', truncated: true })} />);
    expect(view.container.textContent).toContain('已在 64 KiB 处截断');
    view.unmount();
  });

  it('renders the machine error code of a failed tool', () => {
    const view = mount(
      <ToolCard
        row={toolRow({ output: 'permission denied', ok: false, errorCode: 'sandbox_denied' })}
      />,
    );
    expect(view.container.textContent).toContain('sandbox_denied');
    expect(view.container.querySelector('.error-code')?.textContent).toBe('sandbox_denied');
    expect(view.container.textContent).toContain('失败');
    view.unmount();
  });

  it('shows a running card before the result arrives', () => {
    const view = mount(<ToolCard row={toolRow({ done: false, output: undefined })} />);
    expect(view.container.textContent).toContain('运行中');
    expect(view.container.querySelector('.tool-output')).toBeNull();
    view.unmount();
  });

  it('expands collapsed output to the full text on demand', () => {
    const lines = Array.from({ length: 100 }, (_, index) => `line-${String(index + 1)}`);
    const view = mount(<ToolCard row={toolRow({ output: lines.join('\n') })} />);
    // 成功的调用默认收成一行，先把它展开才有正文可看。
    const head = view.container.querySelector('.tool-card-toggle');
    interact(() => head?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    const output = (): string => view.container.querySelector('.tool-output')?.textContent ?? '';
    expect(output()).toContain('已隐藏');
    const button = view.container.querySelector('.link-button');
    expect(button?.textContent).toBe('显示全部 100 行');
    interact(() => button?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(output()).toContain('line-50');
    expect(output()).not.toContain('lines hidden');
    view.unmount();
  });

  it('opens closed when the call succeeded and opens itself when it failed', () => {
    const ok = mount(<ToolCard row={toolRow({ output: 'a\nb' })} />);
    const okHead = ok.container.querySelector('.tool-card-toggle');
    expect(okHead?.getAttribute('aria-expanded')).toBe('false');
    expect(ok.container.querySelector('.tool-output')).toBeNull();
    // 一行里要说清这次调用动的是什么，只写工具名等于没写。
    expect(okHead?.textContent).toContain('cat big.log');
    ok.unmount();

    const broken = mount(<ToolCard row={toolRow({ output: 'permission denied', ok: false })} />);
    const brokenHead = broken.container.querySelector('.tool-card-toggle');
    expect(brokenHead?.getAttribute('aria-expanded')).toBe('true');
    expect(broken.container.querySelector('.tool-output')?.textContent).toContain('permission denied');
    broken.unmount();
  });

  it('closes again when the reader clicks the line', () => {
    const view = mount(<ToolCard row={toolRow({ output: 'a\nb' })} />);
    const head = (): Element | null => view.container.querySelector('.tool-card-toggle');
    interact(() => head()?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(head()?.getAttribute('aria-expanded')).toBe('true');
    interact(() => head()?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(head()?.getAttribute('aria-expanded')).toBe('false');
    view.unmount();
  });
});

describe('argHint', () => {
  it('takes the first string argument and flattens its whitespace', () => {
    expect(argHint({ cmd: 'cargo  test\n--offline' })).toBe('cargo test --offline');
  });

  it('is empty when there is nothing a reader could recognise', () => {
    expect(argHint(undefined)).toBe('');
    expect(argHint({ n: 3, ok: true })).toBe('');
  });

  it('truncates a long command so the line stays one line', () => {
    const hint = argHint({ cmd: 'x'.repeat(200) });
    expect(hint).toHaveLength(61);
    expect(hint.endsWith('…')).toBe(true);
  });
});