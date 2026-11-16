import { describe, expect, it, vi } from 'vitest';
import type { ApprovalDecision, PendingApproval } from '../api/types';
import { ApprovalDialog } from './ApprovalDialog';
import { flush, interact, mount, typeInto } from '../test/render';

const pending: PendingApproval = {
  approvalId: 'apr-1',
  callId: 'call-9',
  tool: 'shell',
  args: { cmd: 'rm -rf build' },
  reason: 'shell writes outside the workspace sandbox',
  requestedAt: 1_700_000_000_000,
};

function click(container: HTMLElement, label: string): void {
  interact(() => {
    const button = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.textContent === label,
    );
    if (button === undefined) throw new Error(`no button labelled ${label}`);
    button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  });
}

describe('ApprovalDialog', () => {
  it('shows the tool, callId and args it is asking about', () => {
    const view = mount(<ApprovalDialog pending={pending} onDecide={() => undefined} />);
    const text = view.container.textContent ?? '';
    expect(text).toContain('shell');
    expect(text).toContain('call-9');
    expect(text).toContain('apr-1');
    expect(text).toContain('rm -rf build');
    view.unmount();
  });

  it('cannot double-submit: two clicks in one tick reach onDecide once', async () => {
    const decided: ApprovalDecision[] = [];
    let resolve: (() => void) | undefined;
    const gate = new Promise<void>((done) => {
      resolve = done;
    });
    const onDecide = (decision: ApprovalDecision): Promise<void> => {
      decided.push(decision);
      return gate;
    };

    const view = mount(<ApprovalDialog pending={pending} onDecide={onDecide} />);
    click(view.container, 'Allow');
    expect(decided).toEqual(['allow']);

    // Buttons are disabled while the request is in flight, and their label says so.
    expect(view.container.textContent).toContain('submitting…');
    const buttons = Array.from(view.container.querySelectorAll('button'));
    expect(buttons.length).toBeGreaterThan(0);
    expect(buttons.every((button) => (button as HTMLButtonElement).disabled)).toBe(true);

    // Even if a click is dispatched past the disabled attribute (a synthetic
    // click, or a second event already queued in the same tick), the ref guard
    // keeps it from reaching the network.
    interact(() => {
      for (const button of buttons) {
        button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      }
    });
    expect(decided).toEqual(['allow']);

    resolve?.();
    await flush();
    view.unmount();
  });

  it('sends the optional note with the decision', async () => {
    const calls: { decision: ApprovalDecision; note?: string }[] = [];
    const view = mount(
      <ApprovalDialog
        pending={pending}
        onDecide={(decision, note) => {
          calls.push(note === undefined ? { decision } : { decision, note });
        }}
      />,
    );
    const textarea = view.container.querySelector('textarea');
    if (textarea === null) throw new Error('no note textarea');
    typeInto(textarea, 'only inside build/');
    click(view.container, 'Deny');
    await flush();
    expect(calls).toEqual([{ decision: 'deny', note: 'only inside build/' }]);
    view.unmount();
  });

  it('surfaces a failed decision and re-enables the buttons', async () => {
    const onDecide = vi.fn(() => Promise.reject(new Error('conflict: already decided')));
    const view = mount(<ApprovalDialog pending={pending} onDecide={onDecide} />);
    click(view.container, 'Allow');
    await flush();
    expect(view.container.textContent).toContain('conflict: already decided');
    const buttons = Array.from(view.container.querySelectorAll('button'));
    expect(buttons.every((button) => (button as HTMLButtonElement).disabled)).toBe(false);
    view.unmount();
  });
});