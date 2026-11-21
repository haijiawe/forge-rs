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
  const connection = needsResync ? '重新同步中' : connecting ? '连接中' : '在线';
  return (
    <footer className="status-bar">
      <span className={`status-dot status-${status}`} aria-hidden="true" />
      <span className="status-item status-mode">{status}</span>
      <span className="status-item" title="模型">{session?.model ?? '—'}</span>
      <span className="status-item mono" title="会话 id">{shortId(sessionId)}</span>
      <span className="status-item mono" title="最后应用的事件序号">
        序号 {String(lastSeq)}
      </span>
      <span className={`status-item conn conn-${connection}`}>{connection}</span>
      {streamError !== null && <span className="status-item status-error">{streamError}</span>}
    </footer>
  );
}