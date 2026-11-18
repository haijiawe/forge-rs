import { describe, expect, it } from 'vitest';
import { groupOf, latestToolNames, sectionsFor } from './capabilities';

describe('groupOf', () => {
  it('groups the harness vocabulary by what each tool does', () => {
    expect(groupOf('fs_read')).toBe('files');
    expect(groupOf('fs_grep')).toBe('files');
    expect(groupOf('shell')).toBe('process');
    expect(groupOf('todo_write')).toBe('notes');
    expect(groupOf('present')).toBe('notes');
    expect(groupOf('web_fetch')).toBe('network');
    expect(groupOf('skill')).toBe('skills');
    expect(groupOf('subagent')).toBe('delegation');
    expect(groupOf('team')).toBe('platform');
    expect(groupOf('expert')).toBe('platform');
  });

  it('recognises the two shapes the wiring layer imposes', () => {
    // `mcp__server__tool` and `plugin.tool` are not vocabulary, they are forms.
    expect(groupOf('mcp__github__search')).toBe('servers');
    expect(groupOf('reference.echo')).toBe('plugins');
    // A name with no dot and no prefix is vocabulary or nothing.
    expect(groupOf('something_new')).toBe('other');
  });

  it('shows an unknown name rather than hiding it', () => {
    // A panel that drops what it does not understand would claim the request
    // was smaller than it was.
    expect(groupOf('mystery_tool')).toBe('other');
  });
});

describe('sectionsFor', () => {
  it('returns nothing for no names', () => {
    expect(sectionsFor([])).toEqual([]);
  });

  it('orders sections the way the order table says, not by input order', () => {
    const sections = sectionsFor(['reference.echo', 'mcp__x__y', 'fs_read']);
    expect(sections.map((section) => section.group)).toEqual([
      'files',
      'servers',
      'plugins',
    ]);
  });

  it('sorts tool names inside a section so the picture is stable', () => {
    const sections = sectionsFor(['fs_write', 'fs_read', 'fs_grep']);
    expect(sections).toHaveLength(1);
    expect(sections[0]?.tools.map((tool) => tool.name)).toEqual([
      'fs_grep',
      'fs_read',
      'fs_write',
    ]);
  });

  it('keeps every name, including duplicates from a later request', () => {
    const sections = sectionsFor(['fs_read', 'fs_read', 'mystery_tool']);
    const names = sections.flatMap((section) => section.tools.map((tool) => tool.name));
    expect(names).toContain('fs_read');
    expect(names).toContain('mystery_tool');
  });

  it('ignores blank names instead of rendering an empty chip', () => {
    const sections = sectionsFor(['', '  ', 'shell']);
    expect(sections).toHaveLength(1);
    expect(sections[0]?.tools.map((tool) => tool.name)).toEqual(['shell']);
  });
});

describe('latestToolNames', () => {
  it('returns nothing before any request', () => {
    expect(latestToolNames([])).toEqual([]);
    expect(latestToolNames([{ type: 'session/start', payload: {} }])).toEqual([]);
  });

  it('reads the tools member of the last llm/call', () => {
    const events = [
      { type: 'llm/call', payload: { tools: ['fs_read', 'shell'] } },
      { type: 'tool/result', payload: {} },
    ];
    expect(latestToolNames(events)).toEqual(['fs_read', 'shell']);
  });

  it('takes the newest request, because the list can change between turns', () => {
    const events = [
      { type: 'llm/call', payload: { tools: ['fs_read'] } },
      { type: 'llm/call', payload: { tools: ['fs_read', 'reference.echo'] } },
    ];
    expect(latestToolNames(events)).toEqual(['fs_read', 'reference.echo']);
  });

  it('survives a payload that is missing, wrong-typed or not an object', () => {
    const events = [
      { type: 'llm/call' },
      { type: 'llm/call', payload: null },
      { type: 'llm/call', payload: { tools: 'fs_read' } },
      { type: 'llm/call', payload: { tools: [1, null, 'fs_read'] } },
    ];
    // The last one has a real name in a bad array: the name survives, the junk
    // does not.
    expect(latestToolNames(events)).toEqual(['fs_read']);
  });
});
