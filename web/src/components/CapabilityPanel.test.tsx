import { describe, expect, it } from 'vitest';
import { CapabilityPanel } from './CapabilityPanel';
import { mount } from '../test/render';

function render(names: string[]) {
  const mounted = mount(<CapabilityPanel names={names} />);
  return mounted;
}

describe('CapabilityPanel', () => {
  it('says plainly that there is nothing to report before a request', () => {
    const { container, unmount } = render([]);
    expect(container.textContent).toContain('no llm/call event yet');
    expect(container.textContent).toContain('not requested yet');
    // It must not render an empty table that reads like "no tools exist".
    expect(container.querySelectorAll('.capability-tool')).toHaveLength(0);
    unmount();
  });

  it('renders every name the request carried', () => {
    const { container, unmount } = render(['fs_read', 'shell', 'reference.echo']);
    const chips = Array.from(container.querySelectorAll('.capability-tool')).map(
      (chip) => chip.textContent,
    );
    expect(chips).toEqual(['fs_read', 'shell', 'reference.echo']);
    expect(container.textContent).toContain('3 in the last request');
    unmount();
  });

  it('groups plugin and mcp tools under their own headings', () => {
    const { container, unmount } = render(['mcp__github__search', 'reference.echo']);
    expect(container.textContent).toContain('mcp servers');
    expect(container.textContent).toContain('plugins');
    // The group heading carries the count, so a collapsed section still says
    // how much it holds.
    expect(container.textContent).toContain('child processes speaking the plugin protocol');
    unmount();
  });

  it('keeps an unknown tool visible instead of dropping it', () => {
    const { container, unmount } = render(['mystery_tool']);
    expect(container.textContent).toContain('mystery_tool');
    expect(container.textContent).toContain('other');
    unmount();
  });

  it('gives the two P3 platform tools their own section', () => {
    const { container, unmount } = render(['expert', 'team', 'fs_read']);
    expect(container.textContent).toContain('platform (experts & teams)');
    const chips = Array.from(container.querySelectorAll('.capability-tool')).map(
      (chip) => chip.textContent,
    );
    expect(chips).toContain('expert');
    expect(chips).toContain('team');
    unmount();
  });

  it('says a P3 tool was not offered rather than hinting at a hidden roster', () => {
    const { container, unmount } = render(['fs_read', 'shell']);
    expect(container.textContent).toContain('neither expert nor team was offered');
    unmount();
  });

  it('states the one thing the browser cannot know, in words', () => {
    const { container, unmount } = render(['expert']);
    // No endpoint for installed-but-unused plugins or rosters: the pane has to
    // say so on screen, and point at the CLI that can.
    expect(container.textContent).toContain('forge plugin list');
    expect(container.textContent).toContain('forge expert list');
    expect(container.textContent).toContain('no endpoint');
    unmount();
  });
});
