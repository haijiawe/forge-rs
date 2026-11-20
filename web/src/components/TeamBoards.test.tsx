// The board view, tested through its rendered output: the list has to answer
// "did my team work" before anything is clicked, and the one state the API
// cannot decide (`ok: null`) must not be rounded to success or failure.
import { describe, expect, it } from 'vitest';
import { ApiError } from '../api/client';
import { TeamBoards } from './TeamBoards';
import { interact, mount, waitFor } from '../test/render';

const BOARD = 'demo-20261120-000000';

function baseBoard(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    board: BOARD,
    team: 'demo',
    parallel: null,
    createdAt: '2026-11-20T00:00:00Z',
    tasks: [
      { id: 't1', subject: 'do a thing', status: 'completed', owner: 'alpha' },
      { id: 't2', subject: 'do another', status: 'in_progress', owner: null },
      { id: 't3', subject: 'do a third', status: 'pending', owner: 'beta' },
    ],
    workers: [
      { name: 'alpha', ok: true, note: '1 turn(s)', events: 2 },
      { name: 'beta', ok: false, note: 'tool failed', events: 2 },
      { name: 'gamma', ok: null, note: '', events: 1 },
    ],
    ...overrides,
  };
}

async function renderLoad(load: () => Promise<unknown>) {
  const view = mount(<TeamBoards load={load} />);
  await waitFor(() => !(view.container.textContent ?? '').includes('reading the team boards'));
  return view;
}

function text(container: HTMLElement): string {
  return container.textContent ?? '';
}

/** The one toggle button of a single-board payload. */
function toggle(view: { container: HTMLElement }): HTMLButtonElement {
  const found = view.container.querySelector<HTMLButtonElement>('.team-board-toggle');
  if (found === null) throw new Error('no .team-board-toggle rendered');
  return found;
}

describe('TeamBoards', () => {
  it('answers "did my team work" from the list, before any click', async () => {
    const view = await renderLoad(() => Promise.resolve({ boards: [baseBoard()] }));
    const rendered = text(view.container);

    expect(rendered).toContain('demo');
    expect(rendered).toContain(BOARD);
    expect(rendered).toContain('2026-11-20T00:00:00Z');
    // Task counts by status, worker counts by the three states: all on the row.
    expect(view.container.querySelector('.team-board-tasks')?.textContent).toContain(
      'pending 1 · in_progress 1 · completed 1',
    );
    expect(view.container.querySelector('.team-board-workers')?.textContent).toContain(
      'finished 1 · failed 1 · unknown 1',
    );
    // Nothing is expanded until it is asked for.
    expect(view.container.querySelector('.team-board-detail')).toBeNull();
    view.unmount();
  });

  it('renders a finished worker as finished', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [baseBoard({ workers: [{ name: 'alpha', ok: true, note: '1 turn(s)', events: 2 }] })],
      }),
    );
    interact(() => toggle(view).click());
    const pill = view.container.querySelector('.team-worker-state');
    expect(pill?.className).toContain('team-worker-state-finished');
    expect(pill?.textContent).toBe('finished');
    // The other two states are rendered as zero counts, not as absent.
    expect(view.container.querySelector('.team-worker-count-failed')?.textContent).toBe(
      'failed 0',
    );
    expect(view.container.querySelector('.team-worker-count-unknown')?.textContent).toBe(
      'unknown 0',
    );
    view.unmount();
  });

  it('renders a failed worker as failed', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [baseBoard({ workers: [{ name: 'beta', ok: false, note: 'tool failed', events: 2 }] })],
      }),
    );
    interact(() => toggle(view).click());
    const pill = view.container.querySelector('.team-worker-state');
    expect(pill?.className).toContain('team-worker-state-failed');
    expect(pill?.textContent).toBe('failed');
    expect(view.container.querySelector('.team-worker-state-finished')).toBeNull();
    expect(view.container.querySelector('.team-worker-count-finished')?.textContent).toBe(
      'finished 0',
    );
    view.unmount();
  });

  it('renders ok: null as its own unknown state, neither success nor failure', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [baseBoard({ workers: [{ name: 'gamma', ok: null, note: '', events: 1 }] })],
      }),
    );
    // Before expanding: the count row already refuses to call it either verdict.
    expect(view.container.querySelector('.team-worker-count-unknown')?.textContent).toBe(
      'unknown 1',
    );
    expect(view.container.querySelector('.team-worker-count-finished')?.textContent).toBe(
      'finished 0',
    );
    expect(view.container.querySelector('.team-worker-count-failed')?.textContent).toBe('failed 0');

    interact(() => toggle(view).click());
    const pill = view.container.querySelector('.team-worker-state');
    expect(pill?.className).toContain('team-worker-state-unknown');
    expect(pill?.className).not.toContain('team-worker-state-finished');
    expect(pill?.className).not.toContain('team-worker-state-failed');
    expect(pill?.textContent).toBe('unknown');
    // And it says what evidence is missing instead of leaving the reader to guess.
    expect(text(view.container)).toContain('no worker/end line was written');
    view.unmount();
  });

  it('says no team has run here when the board list is empty, and calls it not an error', async () => {
    const view = await renderLoad(() => Promise.resolve({ boards: [] }));
    const rendered = text(view.container);
    expect(rendered).toContain('no team has run');
    expect(rendered).toContain('not a failure');
    expect(view.container.querySelector('.teams-empty')).not.toBeNull();
    expect(view.container.querySelector('.team-board')).toBeNull();
    // An empty list is not the failed-read state.
    expect(view.container.querySelector('.banner-error')).toBeNull();
    view.unmount();
  });

  it('says a board with no tasks has no tasks, without inventing progress', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [baseBoard({ tasks: [], workers: [{ name: 'alpha', ok: true, note: '', events: 2 }] })],
      }),
    );
    const rendered = text(view.container);
    expect(rendered).toContain('no tasks on this board');
    expect(view.container.querySelector('.team-board-tasks-empty')).not.toBeNull();
    // No percentage, no bar, no denominator this build could not know.
    expect(rendered).not.toContain('%');
    expect(view.container.querySelector('.banner-error')).toBeNull();
    expect(view.container.querySelector('.team-board')).not.toBeNull();
    view.unmount();
  });

  it('shows the error code and points at the CLI when the read fails', async () => {
    const view = await renderLoad(() =>
      Promise.reject(new ApiError('log_read_failed', 'teams directory is unreadable', 500)),
    );
    const rendered = text(view.container);
    expect(rendered).toContain('log_read_failed');
    expect(rendered).toContain('hearth team list');
    expect(view.container.querySelector('.banner-error')?.getAttribute('role')).toBe('alert');
    // A failed read shows no boards at all: none of them is claimed to be current.
    expect(view.container.querySelector('.team-board')).toBeNull();
    view.unmount();
  });

  it('renders a malformed answer as an empty list instead of crashing', async () => {
    for (const body of [null, 'not json', 42, { boards: [{ team: 'nameless' }] }, {}]) {
      const view = await renderLoad(() => Promise.resolve(body));
      expect(view.container.querySelector('.team-board')).toBeNull();
      expect(text(view.container)).toContain('no team has run');
      expect(view.container.querySelector('.banner-error')).toBeNull();
      view.unmount();
    }
  });

  it('renders a board that has no workers key at all', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [{ board: 'half-20261120-000001', team: 'half', createdAt: '2026-11-20T00:00:01Z', tasks: [] }],
      }),
    );
    expect(view.container.querySelector('.team-board')).not.toBeNull();
    expect(view.container.querySelector('.team-board-workers')?.textContent).toContain(
      'finished 0 · failed 0 · unknown 0',
    );
    interact(() => toggle(view).click());
    expect(text(view.container)).toContain('no worker transcripts to show');
    view.unmount();
  });

  it('expands a board from a button with an accessible name and a real control target', async () => {
    const view = await renderLoad(() => Promise.resolve({ boards: [baseBoard()] }));
    // A list a keyboard can move through: one focusable control per board.
    expect(view.container.querySelector('ul.teams-list')).not.toBeNull();
    expect(view.container.querySelectorAll('ul.teams-list > li.team-board')).toHaveLength(1);

    const button = toggle(view);
    expect(button.tagName).toBe('BUTTON');
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(button.getAttribute('aria-label')).toBe(`show details for ${BOARD}`);
    const controls = button.getAttribute('aria-controls');
    expect(controls).not.toBeNull();
    expect(document.getElementById(controls ?? '')).toBeNull();

    interact(() => button.click());
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(button.getAttribute('aria-label')).toBe(`hide details for ${BOARD}`);
    const detail = document.getElementById(controls ?? '');
    expect(detail).not.toBeNull();
    // The detail is the data already in hand: three tasks and three workers.
    expect(detail?.querySelectorAll('tbody tr')).toHaveLength(6);

    interact(() => button.click());
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(view.container.querySelector('.team-board-detail')).toBeNull();
    view.unmount();
  });

  it('keeps the order the server sent instead of re-sorting', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [
          // Deliberately oldest first: the component must not re-order what it
          // was handed, because the server defines "newest first".
          { board: 'old-20261119-000000', team: 'old', createdAt: '2026-11-19T00:00:00Z', tasks: [], workers: [] },
          { board: 'new-20261120-000000', team: 'new', createdAt: '2026-11-20T00:00:00Z', tasks: [], workers: [] },
        ],
      }),
    );
    const ids = Array.from(view.container.querySelectorAll('.team-board-id')).map(
      (element) => element.textContent,
    );
    expect(ids).toEqual(['old-20261119-000000', 'new-20261120-000000']);
    view.unmount();
  });

  it('renders parallelism as unknown rather than a number nothing recorded', async () => {
    const view = await renderLoad(() => Promise.resolve({ boards: [baseBoard()] }));
    const parallel = view.container.querySelector('.team-board-parallel');
    expect(parallel?.textContent).toBe('unknown');
    expect(parallel?.className).toContain('team-board-parallel-unknown');
    expect(text(view.container)).not.toContain('parallelism 0');
    view.unmount();
  });

  it('shows a diagnostics badge on the row and the strings only when expanded', async () => {
    const view = await renderLoad(() =>
      Promise.resolve({
        boards: [
          baseBoard({
            diagnostics: ['workers/alpha.jsonl has no worker/end line: the worker had not finished'],
          }),
        ],
      }),
    );
    const badge = view.container.querySelector('.team-board-diagnostic-badge');
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toContain('1 diagnostic');
    // Collapsed, the human strings are not spammed into the list.
    expect(text(view.container)).not.toContain('had not finished');
    interact(() => toggle(view).click());
    expect(text(view.container)).toContain('had not finished');
    view.unmount();
  });
});