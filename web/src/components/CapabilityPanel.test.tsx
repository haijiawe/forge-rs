import { describe, expect, it } from 'vitest';
import { CapabilityPanel } from './CapabilityPanel';
import type { CapabilityReport } from '../api/capabilities';
import { mount } from '../test/render';

function render(names: string[], report?: CapabilityReport | null, reportError?: string | null) {
  const mounted = mount(
    <CapabilityPanel names={names} report={report ?? null} reportError={reportError ?? null} />,
  );
  return mounted;
}

function emptyReport(overrides: Partial<CapabilityReport> = {}): CapabilityReport {
  return {
    mode: 'workspace-write',
    workspace: 'C:/work',
    model: 'm1',
    plugins: [],
    experts: [],
    teams: [],
    bundles: [],
    skills: [],
    mcp: [],
    ...overrides,
  };
}

describe('CapabilityPanel', () => {
  it('says plainly that there is nothing to report before a request', () => {
    const { container, unmount } = render([]);
    expect(container.textContent).toContain('本次会话还没有');
    expect(container.textContent).toContain('还没有发起请求');
    // It must not render an empty table that reads like "no tools exist".
    expect(container.querySelectorAll('.capability-tool')).toHaveLength(0);
    unmount();
  });

  it('renders every name the request carried', () => {
    const { container, unmount } = render(['fs_read', 'shell', 'reference.echo']);
    const chips = Array.from(container.querySelectorAll('.capability-tool')).map(
      (chip) => chip.textContent,
    );
    expect(chips).toContain('fs_read');
    expect(chips).toContain('shell');
    expect(chips).toContain('reference.echo');
    expect(container.textContent).toContain('上次请求共 3 个');
    unmount();
  });

  it('groups plugin and mcp tools under their own headings', () => {
    const { container, unmount } = render(['mcp__github__search', 'reference.echo']);
    expect(container.textContent).toContain('MCP 服务器');
    expect(container.textContent).toContain('插件');
    // The group heading carries the count, so a collapsed section still says
    // how much it holds.
    expect(container.textContent).toContain('用插件协议通信的子进程');
    unmount();
  });

  it('keeps an unknown tool visible instead of dropping it', () => {
    const { container, unmount } = render(['mystery_tool']);
    expect(container.textContent).toContain('mystery_tool');
    expect(container.textContent).toContain('其他');
    unmount();
  });

  it('gives the two P3 platform tools their own section', () => {
    const { container, unmount } = render(['expert', 'team', 'fs_read']);
    expect(container.textContent).toContain('平台（专家与团队）');
    const chips = Array.from(container.querySelectorAll('.capability-tool')).map(
      (chip) => chip.textContent,
    );
    expect(chips).toContain('expert');
    expect(chips).toContain('team');
    unmount();
  });

  it('says a P3 tool was not offered rather than hinting at a hidden roster', () => {
    const { container, unmount } = render(['fs_read', 'shell']);
    expect(container.textContent).toContain('都没有出现在这次请求里');
    unmount();
  });

  it('lists a configured plugin that was not offered, and says which it is', () => {
    const report = emptyReport({
      plugins: [
        { name: 'counter', detail: '1.0.0 (node runtime)', active: true },
        { name: 'ghost', detail: '0.1.0 (node runtime), not enabled', active: false },
      ],
    });
    const { container, unmount } = render(['fs_read'], report);
    expect(container.textContent).toContain('已配置');
    expect(container.textContent).toContain('1 项未提供');
    const off = container.querySelectorAll('.capability-tool-off');
    expect(off).toHaveLength(1);
    expect(off[0]?.textContent).toContain('ghost');
    expect(off[0]?.textContent).toContain('（未提供）');
    // The active one must not carry the warning suffix.
    const on = Array.from(container.querySelectorAll('.capability-tool')).find((chip) =>
      chip.textContent?.startsWith('counter'),
    );
    expect(on?.textContent).toBe('counter');
    unmount();
  });

  it('says a configuration with nothing unused is a configuration, not a failure', () => {
    const { container, unmount } = render(['fs_read'], emptyReport());
    expect(container.textContent).toContain('没有任何已配置但未被使用的项');
    unmount();
  });

  it('points at the CLI when the configured set could not be read', () => {
    const { container, unmount } = render(['expert'], null, 'connection refused');
    expect(container.textContent).toContain('connection refused');
    expect(container.textContent).toContain('hearth plugin list');
    unmount();
  });

  it('waits quietly before the configured set arrives', () => {
    const { container, unmount } = render(['fs_read']);
    expect(container.textContent).toContain('正在读取已配置的集合');
    unmount();
  });
});