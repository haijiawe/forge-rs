import { describe, expect, it } from 'vitest';
import { COLLAPSED_LINES, collapseOutput } from './ToolCard';
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
    expect(result.text).toContain('… 60 lines hidden …');
    expect(result.text).toContain('line-100');
    // The middle is gone, not just visually hidden.
    expect(result.text).not.toContain('line-50\n');
  });
});

describe('ToolCard', () => {
  it('renders the truncation marker the server set', () => {
    const view = mount(<ToolCard row={toolRow({ output: 'a\nb', truncated: true })} />);
    expect(view.container.textContent).toContain('truncated at 64 KiB');
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
    expect(view.container.textContent).toContain('failed');
    view.unmount();
  });

  it('shows a running card before the result arrives', () => {
    const view = mount(<ToolCard row={toolRow({ done: false, output: undefined })} />);
    expect(view.container.textContent).toContain('running');
    expect(view.container.querySelector('.tool-output')).toBeNull();
    view.unmount();
  });

  it('expands collapsed output to the full text on demand', () => {
    const lines = Array.from({ length: 100 }, (_, index) => `line-${String(index + 1)}`);
    const view = mount(<ToolCard row={toolRow({ output: lines.join('\n') })} />);
    const output = (): string => view.container.querySelector('.tool-output')?.textContent ?? '';
    expect(output()).toContain('lines hidden');
    const button = view.container.querySelector('.link-button');
    expect(button?.textContent).toBe('show all 100 lines');
    interact(() => button?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(output()).toContain('line-50');
    expect(output()).not.toContain('lines hidden');
    view.unmount();
  });
});