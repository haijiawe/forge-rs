import { describe, expect, it } from 'vitest';
import {
  baseName,
  formatBytes,
  formatWhen,
  parsePresent,
  parseReport,
} from './presents';

describe('parsePresent', () => {
  it('keeps every recorded member', () => {
    expect(
      parsePresent({ ts: 1_763_634_000_000, path: 'C:\\out\\a.csv', size: 8, description: 'the numbers' }),
    ).toEqual({
      ts: 1_763_634_000_000,
      path: 'C:\\out\\a.csv',
      size: 8,
      description: 'the numbers',
    });
  });

  it('drops a row with no path, because there is nothing to open', () => {
    // The one member with no defensible default: an empty path is not a file.
    expect(parsePresent({ ts: 1, size: 4 })).toBeNull();
    expect(parsePresent({ path: '' })).toBeNull();
    expect(parsePresent({ path: 7 })).toBeNull();
  });

  it('defaults the other members instead of inventing values', () => {
    expect(parsePresent({ path: 'a.txt' })).toEqual({
      ts: 0,
      path: 'a.txt',
      size: 0,
      description: '',
    });
    // A negative size or stamp is damage, and a default is the honest answer.
    expect(parsePresent({ path: 'a.txt', ts: -5, size: -1 })).toEqual({
      ts: 0,
      path: 'a.txt',
      size: 0,
      description: '',
    });
  });

  it('refuses anything that is not an object', () => {
    expect(parsePresent(null)).toBeNull();
    expect(parsePresent('a.txt')).toBeNull();
    expect(parsePresent([])).toBeNull();
  });
});

describe('parseReport', () => {
  it('keeps the partial-report facts rather than folding them away', () => {
    const report = parseReport({
      presents: [{ path: 'a.csv', size: 8 }],
      skipped: 2,
      truncated: true,
      note: 'the manifest was cut',
    });
    expect(report.presents).toHaveLength(1);
    expect(report.skipped).toBe(2);
    expect(report.truncated).toBe(true);
    expect(report.note).toBe('the manifest was cut');
  });

  it('treats truncated as true only when it is literally true', () => {
    expect(parseReport({ truncated: 'yes' }).truncated).toBe(false);
    expect(parseReport({ truncated: 1 }).truncated).toBe(false);
  });

  it('answers an empty report for a shape it cannot read', () => {
    expect(parseReport(null)).toEqual({
      presents: [],
      skipped: 0,
      truncated: false,
      note: null,
    });
    expect(parseReport('nope')).toEqual({
      presents: [],
      skipped: 0,
      truncated: false,
      note: null,
    });
  });

  it('accepts a bare list, which a hand-written fixture reaches for', () => {
    const report = parseReport([{ path: 'a.csv' }, { nope: true }]);
    expect(report.presents.map((row) => row.path)).toEqual(['a.csv']);
    expect(report.skipped).toBe(0);
  });

  it('turns an empty note into null so the pane can test one thing', () => {
    expect(parseReport({ presents: [], note: '' }).note).toBeNull();
  });
});

describe('formatBytes', () => {
  it('says bytes, kilobytes and megabytes as recorded', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
  });

  it('refuses a number it cannot stand behind', () => {
    expect(formatBytes(-1)).toBe('size unknown');
    expect(formatBytes(Number.NaN)).toBe('size unknown');
  });
});

describe('formatWhen', () => {
  it('prints the recorded stamp in UTC', () => {
    expect(formatWhen(Date.UTC(2026, 10, 20, 9, 41, 1))).toBe('2026-11-20 09:41:01Z');
  });

  it('says the time was not recorded rather than printing 1970', () => {
    // `0` is a fact about the line, not a moment in time.
    expect(formatWhen(0)).toBe('time not recorded');
    expect(formatWhen(-1)).toBe('time not recorded');
  });
});

describe('baseName', () => {
  it('takes the last segment of either separator', () => {
    expect(baseName('C:\\out\\a.csv')).toBe('a.csv');
    expect(baseName('/tmp/a.csv')).toBe('a.csv');
    expect(baseName('a.csv')).toBe('a.csv');
  });
});