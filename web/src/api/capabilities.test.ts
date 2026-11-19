import { describe, expect, it } from 'vitest';
import {
  configuredGroups,
  inactiveCount,
  parseReport,
  type CapabilityReport,
} from './capabilities';

function report(overrides: Partial<CapabilityReport> = {}): CapabilityReport {
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

describe('parseReport', () => {
  it('reads a full report', () => {
    const decoded = parseReport({
      mode: 'full-access',
      workspace: 'C:/w',
      model: 'm2',
      plugins: [{ name: 'counter', detail: '1.0.0', active: true }],
    });
    expect(decoded.mode).toBe('full-access');
    expect(decoded.workspace).toBe('C:/w');
    expect(decoded.model).toBe('m2');
    expect(decoded.plugins).toEqual([{ name: 'counter', detail: '1.0.0', active: true }]);
    // Absent groups are empty, never undefined.
    expect(decoded.experts).toEqual([]);
    expect(decoded.mcp).toEqual([]);
  });

  it('survives a response that is not a report at all', () => {
    for (const value of [null, undefined, 42, 'text', [], true]) {
      const decoded = parseReport(value);
      expect(decoded.mode).toBe('');
      expect(decoded.plugins).toEqual([]);
      expect(decoded.skills).toEqual([]);
    }
  });

  it('treats a missing or non-true active flag as not offered', () => {
    const decoded = parseReport({
      plugins: [
        { name: 'on', active: true },
        { name: 'off', active: false },
        { name: 'odd', active: 'yes' },
        { name: 'absent' },
      ],
    });
    expect(decoded.plugins.map((entry) => entry.active)).toEqual([true, false, false, false]);
  });

  it('drops rows without a name rather than rendering an unlabelled chip', () => {
    const decoded = parseReport({
      plugins: [{ name: '', active: true }, { detail: 'no name' }, { name: 'kept' }],
    });
    expect(decoded.plugins.map((entry) => entry.name)).toEqual(['kept']);
  });
});

describe('configuredGroups', () => {
  it('omits empty groups and keeps a fixed order', () => {
    const groups = configuredGroups(
      report({
        skills: [{ name: 'deploy', detail: 'ship it', active: true }],
        plugins: [{ name: 'counter', detail: '1.0.0', active: true }],
      }),
    );
    expect(groups.map((group) => group.group)).toEqual(['plugins', 'skills']);
  });

  it('keeps the whole order when everything is present', () => {
    const entry = { name: 'x', detail: '', active: true };
    const groups = configuredGroups(
      report({
        plugins: [entry],
        experts: [entry],
        teams: [entry],
        bundles: [entry],
        mcp: [entry],
        skills: [entry],
      }),
    );
    expect(groups.map((group) => group.group)).toEqual([
      'plugins',
      'experts',
      'teams',
      'bundles',
      'mcp',
      'skills',
    ]);
  });

  it('returns nothing for an empty report', () => {
    expect(configuredGroups(report())).toEqual([]);
  });
});

describe('inactiveCount', () => {
  it('counts only the rows that are not offered', () => {
    const entry = (name: string, active: boolean) => ({ name, detail: '', active });
    expect(
      inactiveCount(
        report({
          plugins: [entry('a', true), entry('b', false)],
          experts: [entry('c', false)],
          skills: [entry('d', true)],
        }),
      ),
    ).toBe(2);
  });

  it('is zero for an empty report', () => {
    expect(inactiveCount(report())).toBe(0);
  });
});