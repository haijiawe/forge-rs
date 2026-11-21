// The approval dialog: the one place a human can stop a tool before it runs.
//
// Double-submit is a correctness requirement, not a UI nicety: the server
// answers a second POST for the same approvalId with 409 (api.md §5.3), so a
// double click must not even reach the network. A `ref` guard is used rather
// than state, because two clicks can be dispatched before React re-renders.
import { useMemo, useRef, useState } from 'react';
import type { ApprovalDecision, PendingApproval } from '../api/types';

export interface ApprovalDialogProps {
  pending: PendingApproval;
  /** True while another action (e.g. the previous decision) is in flight. */
  busy?: boolean;
  onDecide: (decision: ApprovalDecision, note?: string) => Promise<void> | void;
}

function formatArgs(args: unknown): string {
  if (args === undefined) return '';
  try {
    return JSON.stringify(args, null, 2);
  } catch {
    return String(args);
  }
}

export function ApprovalDialog({ pending, busy = false, onDecide }: ApprovalDialogProps) {
  const [note, setNote] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const args = useMemo(() => formatArgs(pending.args), [pending.args]);
  const disabled = submitting || busy || inFlight.current;

  const submit = async (decision: ApprovalDecision): Promise<void> => {
    // Synchronous guard: a second click in the same tick never gets past this.
    if (inFlight.current) return;
    inFlight.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await onDecide(decision, note.trim() === '' ? undefined : note.trim());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      inFlight.current = false;
      setSubmitting(false);
    }
  };

  return (
    <div className="approval-backdrop" role="presentation">
      <div className="approval-dialog" role="dialog" aria-modal="true" aria-labelledby="approval-title">
        <h2 id="approval-title">需要审批</h2>
        <p className="approval-reason">{pending.reason ?? '有工具请求执行。'}</p>

        <dl className="approval-meta">
          <dt>工具</dt>
          <dd className="mono">{pending.tool}</dd>
          <dt>callId</dt>
          <dd className="mono">{pending.callId}</dd>
          <dt>approvalId</dt>
          <dd className="mono">{pending.approvalId}</dd>
        </dl>

        {args !== '' && (
          <details className="tool-section" open>
            <summary>参数</summary>
            <pre className="mono tool-args">{args}</pre>
          </details>
        )}

        <label className="approval-note">
          <span>备注（可选）</span>
          <textarea
            className="mono"
            value={note}
            rows={2}
            onChange={(event) => setNote(event.target.value)}
            disabled={disabled}
          />
        </label>

        {error !== null && <p className="form-error">{error}</p>}

        <div className="approval-actions">
          <button type="button" className="button button-danger" disabled={disabled} onClick={() => void submit('deny')}>
            {submitting ? '正在提交…' : '拒绝'}
          </button>
          <button type="button" className="button button-primary" disabled={disabled} onClick={() => void submit('allow')}>
            {submitting ? '正在提交…' : '允许'}
          </button>
        </div>
      </div>
    </div>
  );
}