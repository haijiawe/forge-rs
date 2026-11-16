// The status bar: which model, which session, and how far the stream has got.
// lastSeq is the reconnect cursor as well as a debug aid, so it belongs on
// screen: when it stops moving, the stream is the thing to look at.
import type { SessionDetail } from '../api/types';

export interface StatusBarProps {
  session: SessionDetail | null;
  sessionId: string;
  status: string;
  lastSeq: number;
  connecting: boolean;
  needsResync: boolean;
  streamError: string | null;
}

function shortId(id: string): string {
  return id.length <= 16 ? id : `${id.slice(0, 8)}…${id.slice(-4)}`;
}

export function StatusBar({
  session,
  sessionId,
  status,
  lastSeq,
  connecting,
  needsResync,
  streamError,
}: StatusBarProps) {
  const connection = needsResync ? 'resyncing' : connecting ? 'connecting' : 'live';
  return (
    <footer className="status-bar">
      <span className={`status-dot status-${status}`} aria-hidden="true" />
      <span className="status-item status-mode">{status}</span>
      <span className="status-item" title="model">{session?.model ?? '—'}</span>
      <span className="status-item mono" title="session id">{shortId(sessionId)}</span>
      <span className="status-item mono" title="last applied event seq">
        lastSeq {String(lastSeq)}
      </span>
      <span className={`status-item conn conn-${connection}`}>{connection}</span>
      {streamError !== null && <span className="status-item status-error">{streamError}</span>}
    </footer>
  );
}