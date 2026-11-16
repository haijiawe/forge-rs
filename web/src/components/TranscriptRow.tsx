// One transcript row. These components are memoized on the row object: the row
// projection keeps the object identity of every row that did not change, so a
// streaming append re-renders exactly one row (the one with new text) and the
// virtualizer keeps the height of everything else untouched.
import { memo, useState } from 'react';
import type {
  ApprovalRow,
  AssistantRow,
  ErrorRow,
  EventRow,
  Row,
  SummaryRow,
  ToolRow,
  TurnRow,
  UserRow,
} from '../state/rows';
import { Markdown } from './Markdown';
import { ToolCard } from './ToolCard';

function formatTime(ts: number): string {
  if (ts <= 0) return '';
  const date = new Date(ts);
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function formatJson(value: unknown): string {
  if (value === undefined) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export const UserRowView = memo(function UserRowView({ row }: { row: UserRow }) {
  return (
    <div className="row row-user">
      <div className="user-bubble">
        <div className="row-meta">
          <span>you</span>
          <span>{formatTime(row.ts)}</span>
          {row.attachments > 0 && <span>{String(row.attachments)} attachment(s)</span>}
        </div>
        <p className="user-text">{row.text}</p>
      </div>
    </div>
  );
});

export const AssistantRowView = memo(function AssistantRowView({ row }: { row: AssistantRow }) {
  return (
    <div className={`row row-assistant${row.streaming ? ' row-streaming' : ''}`}>
      <div className="row-meta">
        <span>assistant</span>
        <span>{formatTime(row.ts)}</span>
        {row.stopReason !== undefined && row.stopReason !== 'end_turn' && (
          <span className="row-reason">{row.stopReason}</span>
        )}
        {row.partial === true && <span className="row-reason">partial</span>}
      </div>
      <Markdown text={row.text} />
      {row.streaming && <span className="caret" aria-label="streaming" />}
    </div>
  );
});

export const ApprovalRowView = memo(function ApprovalRowView({ row }: { row: ApprovalRow }) {
  const decided = row.decision !== undefined;
  return (
    <div className={`row row-approval${decided ? '' : ' row-approval-pending'}`}>
      <div className="row-meta">
        <span>approval</span>
        <span className="mono">{row.callId}</span>
        <span className={`pill ${row.decision === 'deny' ? 'pill-bad' : decided ? 'pill-good' : 'pill-muted'}`}>
          {row.decision ?? 'pending'}
        </span>
        {row.note !== undefined && <span>note: {row.note}</span>}
      </div>
      <p>
        <span className="mono">{row.tool}</span>
        {row.reason !== undefined && <span className="muted"> — {row.reason}</span>}
      </p>
      {row.args !== undefined && <pre className="mono tool-args">{formatJson(row.args)}</pre>}
    </div>
  );
});

export const SummaryRowView = memo(function SummaryRowView({ row }: { row: SummaryRow }) {
  return (
    <div className="row row-summary">
      <div className="row-meta">
        <span>compaction summary</span>
        {row.from !== undefined && row.to !== undefined && (
          <span className="mono">
            replaces {String(row.from)}–{String(row.to)}
          </span>
        )}
      </div>
      <Markdown text={row.text} />
    </div>
  );
});

export const ErrorRowView = memo(function ErrorRowView({ row }: { row: ErrorRow }) {
  return (
    <div className="row row-error">
      <div className="row-meta">
        <span>error</span>
        {row.code !== undefined && <span className="error-code">{row.code}</span>}
        {row.retryable === true && <span className="row-reason">retryable</span>}
        <span>{formatTime(row.ts)}</span>
      </div>
      <p className="mono">{row.message}</p>
    </div>
  );
});

export const TurnRowView = memo(function TurnRowView({ row }: { row: TurnRow }) {
  return (
    <div className="row row-turn">
      <span className="turn-rule" />
      <span className="turn-label">
        turn {row.turn}
        {row.phase === 'end' ? ' ended' : ' started'}
        {row.reason !== undefined ? ` (${row.reason})` : ''}
      </span>
      <span className="turn-rule" />
    </div>
  );
});

/**
 * The generic renderer for any event type the transcript has no view for.
 * events-v1.md §6: an old reader must survive a new log, so `future/thing`
 * renders as a titled payload dump instead of disappearing.
 */
export const EventRowView = memo(function EventRowView({ row }: { row: EventRow }) {
  const [open, setOpen] = useState(false);
  const payload = formatJson(row.payload);
  if (row.folded) {
    return (
      <div className="row row-event row-event-folded">
        <button type="button" className="link-button" onClick={() => setOpen((v) => !v)}>
          {open ? '▾' : '▸'} {row.type}
        </button>
        <span className="mono muted">seq {String(row.seq)}</span>
        {open && payload !== '' && <pre className="mono tool-args">{payload}</pre>}
      </div>
    );
  }
  return (
    <div className="row row-event">
      <div className="row-meta">
        <span className="mono event-type">{row.type}</span>
        <span className="mono muted">seq {String(row.seq)}</span>
        <span>{formatTime(row.ts)}</span>
      </div>
      {payload !== '' && <pre className="mono tool-args">{payload}</pre>}
    </div>
  );
});

export const ToolRowView = memo(function ToolRowView({ row }: { row: ToolRow }) {
  return (
    <div className="row row-tool">
      <ToolCard row={row} />
    </div>
  );
});

/** Dispatch on `kind`; the row object identity is the memo key. */
export const TranscriptRow = memo(function TranscriptRow({ row }: { row: Row }) {
  switch (row.kind) {
    case 'user':
      return <UserRowView row={row} />;
    case 'assistant':
      return <AssistantRowView row={row} />;
    case 'tool':
      return <ToolRowView row={row} />;
    case 'approval':
      return <ApprovalRowView row={row} />;
    case 'summary':
      return <SummaryRowView row={row} />;
    case 'error':
      return <ErrorRowView row={row} />;
    case 'turn':
      return <TurnRowView row={row} />;
    case 'event':
      return <EventRowView row={row} />;
  }
});