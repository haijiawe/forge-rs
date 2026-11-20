import { describe, expect, it } from 'vitest';
import { SubagentPanel } from './SubagentPanel';
import type { SubagentRow, SubagentTree } from '../api/subagents';
import { mount } from '../test/render';

function row(overrides: Partial<SubagentRow> = {}): SubagentRow {
  return {
    sessionId: 'child-a',
    name: 'agent-1',
    mode: 'one-shot',
    path: 'lead/agent-1',
    depth: 1,
    children: [],
    ...overrides,
  };
}

function tree(rows: SubagentRow[], count = rows.length): SubagentTree {
  return { session: 'root', count, subagents: rows };
}

describe('SubagentPanel', () => {
  it('stays out of the way when the session never delegated', () => {
    const { container, unmount } = mount(<SubagentPanel tree={tree([])} />);
    expect(container.textContent).toBe('');
    unmount();
  });

  it('renders nothing at all before the first read', () => {
    const { container, unmount } = mount(<SubagentPanel tree={null} />);
    expect(container.textContent).toBe('');
    unmount();
  });

  it('names each child with its settled time', () => {
    const { container, unmount } = mount(
      <SubagentPanel
        tree={tree([
          row({
            sessionId: 'child-a',
            name: 'agent-1',
            timing: { settledMs: 1500, active: false, turns: 2 },
          }),
        ])}
      />,
    );
    const text = container.textContent ?? '';
    expect(text).toContain('1 subagent below this session');
    expect(text).toContain('agent-1');
    expect(text).toContain('settled in 1.5 s, 2 turns');
    unmount();
  });

  it('marks a live child as working', () => {
    const { container, unmount } = mount(
      <SubagentPanel
        tree={tree([
          row({ sessionId: 'child-a', timing: { settledMs: 0, active: true, turns: 1 } }),
        ])}
      />,
    );
    expect(container.textContent).toContain('working');
    unmount();
  });

  it('keeps a broken child visible with its diagnostic', () => {
    const { container, unmount } = mount(
      <SubagentPanel
        tree={tree([
          row({
            sessionId: 'ghost',
            name: 'agent-2',
            diagnostic: { kind: 'unavailable', detail: 'no session log for this child' },
          }),
        ])}
      />,
    );
    const text = container.textContent ?? '';
    expect(text).toContain('agent-2');
    expect(text).toContain('unavailable');
    unmount();
  });

  it('indents by depth so the tree reads as a tree', () => {
    const { container, unmount } = mount(
      <SubagentPanel
        tree={tree(
          [
            row({ sessionId: 'a', name: 'agent-1', depth: 1, children: [row({ sessionId: 'b', name: 'agent-2', depth: 2 })] }),
          ],
          2,
        )}
      />,
    );
    const rows = container.querySelectorAll('.subagent-row');
    expect(rows).toHaveLength(2);
    expect((rows[0] as HTMLElement).style.marginLeft).toBe('0px');
    expect((rows[1] as HTMLElement).style.marginLeft).toBe('12px');
    unmount();
  });

  it('points at the CLI when the read failed', () => {
    const { container, unmount } = mount(
      <SubagentPanel tree={null} error="404 no such endpoint" />,
    );
    const text = container.textContent ?? '';
    expect(text).toContain('could not be read');
    expect(text).toContain('hearth subagent tree');
    unmount();
  });
});