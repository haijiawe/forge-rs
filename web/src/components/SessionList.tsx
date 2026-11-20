// Session list screen: every session the server knows about, with the four
// fields that tell you whether it is worth opening.
import { useCallback, useEffect, useState } from 'react';
import type { ForgeApi } from '../api/client';
import { ApiError } from '../api/client';
import type { SessionSummary } from '../api/types';
import { Presents } from './Presents';
import { TeamBoards } from './TeamBoards';

export interface SessionListProps {
  api: ForgeApi;
  onOpen: (id: string) => void;
}

function formatTime(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '—';
  const date = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function shortWorkspace(workspace: string): string {
  const trimmed = workspace.replace(/[\\/]+$/, '');
  const parts = trimmed.split(/[\\/]/);
  return parts.length <= 2 ? trimmed : `…/${parts.slice(-2).join('/')}`;
}

export function SessionList({ api, onOpen }: SessionListProps) {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(
    async (nextCursor?: string) => {
      setLoading(true);
      setError(null);
      try {
        const page = await api.listSessions(nextCursor === undefined ? {} : { cursor: nextCursor });
        setSessions((previous) =>
          nextCursor === undefined ? page.sessions : [...previous, ...page.sessions],
        );
        setCursor(page.nextCursor);
      } catch (cause) {
        setError(cause instanceof ApiError ? `${cause.code}: ${cause.message}` : String(cause));
      } finally {
        setLoading(false);
      }
    },
    [api],
  );

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <main className="screen screen-list">
      <header className="screen-head">
        <h1>sessions</h1>
        <div className="head-actions">
          <button type="button" className="button" onClick={() => void load()} disabled={loading}>
            {loading ? 'loading…' : 'refresh'}
          </button>
        </div>
      </header>

      {error !== null && (
        <p className="banner banner-error" role="alert">
          {error}
        </p>
      )}

      {sessions.length === 0 && !loading && error === null && (
        <p className="empty">no sessions in this data directory yet</p>
      )}

      {sessions.length > 0 && (
        <table className="session-table">
          <thead>
            <tr>
              <th>id</th>
              <th>workspace</th>
              <th>model</th>
              <th>status</th>
              <th>updated</th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((session) => (
              <tr
                key={session.id}
                tabIndex={0}
                onClick={() => onOpen(session.id)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    onOpen(session.id);
                  }
                }}
              >
                <td className="mono">{session.id}</td>
                <td className="mono" title={session.workspace}>
                  {shortWorkspace(session.workspace)}
                </td>
                <td className="mono">{session.model}</td>
                <td>
                  <span className={`pill pill-${statusTone(session.status)}`}>{session.status}</span>
                </td>
                <td className="mono muted">{formatTime(session.updatedAt)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {cursor !== undefined && cursor !== '' && (
        <button type="button" className="button" onClick={() => void load(cursor)} disabled={loading}>
          load more
        </button>
      )}

      {/* Team boards live here rather than on a session screen because a board
          is not part of any one conversation: `forge team run` writes it under
          the data dir, and it outlives every session that spawned a worker. */}
      <TeamBoards load={() => api.teams()} />

      {/* Handoffs are here for the same reason as team boards: `present`
          appends to one manifest in the data dir, so the list belongs to the
          machine and outlives the session whose model handed a file over. */}
      <Presents load={() => api.presents()} />
    </main>
  );
}

function statusTone(status: string): string {
  if (status === 'running') return 'busy';
  if (status === 'awaiting_approval') return 'warn';
  if (status === 'error' || status === 'failed') return 'bad';
  if (status === 'ended' || status === 'idle') return 'good';
  return 'muted';
}