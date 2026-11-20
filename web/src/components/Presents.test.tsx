import { describe, expect, it } from 'vitest';
import { Presents } from './Presents';
import { interact, mount, waitFor } from '../test/render';

async function renderLoad(load: () => Promise<unknown>) {
  const view = mount(<Presents load={load} />);
  await waitFor(() => !(view.container.textContent ?? '').includes('reading the manifest'));
  return view;
}

const ROWS = {
  presents: [
    {
      ts: Date.UTC(2026, 10, 20, 9, 41, 1),
      path: 'D:\\out\\report.csv',
      size: 2048,
      description: 'the numbers you asked for',
    },
    {
      ts: 0,
      path: 'D:\\out\\older.md',
      size: 12,
      description: '',
    },
  ],
  skipped: 1,
  truncated: false,
  note: null,
};

describe('Presents', () => {
  it('lists handoffs in the order the server sent them', async () => {
    const view = await renderLoad(() => Promise.resolve(ROWS));
    const text = view.container.textContent ?? '';
    expect(text).toContain('report.csv');
    expect(text).toContain('2.0 KB');
    expect(text).toContain('the numbers you asked for');
    // Second row: no description, no recorded time.
    expect(text).toContain('older.md');
    expect(text).toContain('time not recorded');
    const names = [...view.container.querySelectorAll('.present-name')].map(
      (node) => node.textContent,
    );
    // Newest first is the server's order and the pane's job is to keep it.
    expect(names).toEqual(['report.csv', 'older.md']);
    view.unmount();
  });

  it('shows the whole path as selectable text, never as a link', async () => {
    const view = await renderLoad(() => Promise.resolve(ROWS));
    const path = view.container.querySelector('.present-path');
    expect(path?.textContent).toBe('D:\\out\\report.csv');
    // A `file:` link is blocked by the browser; an anchor here would be a lie.
    expect(view.container.querySelector('a')).toBeNull();
    view.unmount();
  });

  it('says the list is partial when lines were skipped', async () => {
    const view = await renderLoad(() => Promise.resolve(ROWS));
    const partial = view.container.querySelector('.presents-partial');
    expect(partial?.textContent).toContain('1 line could not be read');
    view.unmount();
  });

  it('says the list is cut short when the server truncated it', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({ ...ROWS, skipped: 0, truncated: true, note: 'the manifest was cut' }),
    );
    const partial = view.container.querySelector('.presents-partial');
    expect(partial?.textContent).toContain('cut short');
    expect(partial?.textContent).toContain('the manifest was cut');
    view.unmount();
  });

  it('separates "nothing presented" from "could not read"', async () => {
    const empty = await renderLoad(() =>
      Promise.resolve({ presents: [], skipped: 0, truncated: false, note: null }),
    );
    expect(empty.container.querySelector('.presents-empty')).not.toBeNull();
    expect(empty.container.querySelector('.banner-error')).toBeNull();
    empty.unmount();

    const broken = await renderLoad(() => Promise.reject(new Error('boom')));
    // A failure is its own state: no empty-list sentence, which would read as
    // "nothing was presented" when the truth is "nothing could be read".
    expect(broken.container.querySelector('.presents-empty')).toBeNull();
    const banner = broken.container.querySelector('.banner-error');
    expect(banner?.textContent).toContain('boom');
    expect(banner?.textContent).toContain('nothing is shown');
    broken.unmount();
  });

  it('re-reads only when asked', async () => {
    let calls = 0;
    const view = await renderLoad(() => {
      calls += 1;
      return Promise.resolve(ROWS);
    });
    expect(calls).toBe(1);
    const retry = view.container.querySelector('.presents-retry');
    expect(retry).not.toBeNull();
    interact(() => {
      (retry as HTMLElement).click();
    });
    await waitFor(() => calls === 2);
    expect(calls).toBe(2);
    view.unmount();
  });
});