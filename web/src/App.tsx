// The app shell: hash routing (no server rewrite rules needed) between the
// session list and one session, plus the token the client was built with.
//
// A hash route is deliberate: this bundle is served from a Go binary at '/',
// so /s/<id> would need a fallback route on the server; '#/s/<id>' does not.
import { useCallback, useEffect, useMemo, useState } from 'react';
import { HearthClient, captureToken } from './api/client';
import { SessionList } from './components/SessionList';
import { SessionView } from './components/SessionView';

interface Route {
  name: 'list';
}

interface SessionRoute {
  name: 'session';
  id: string;
}

type AppRoute = Route | SessionRoute;

function parseRoute(hash: string): AppRoute {
  const match = /^#\/s\/(.+)$/.exec(hash);
  if (match !== null && match[1] !== undefined && match[1] !== '') {
    return { name: 'session', id: decodeURIComponent(match[1]) };
  }
  return { name: 'list' };
}

export interface AppProps {
  /** Injected in tests; production uses the URL/sessionStorage token. */
  baseUrl?: string;
}

export function App({ baseUrl }: AppProps) {
  // Read once, on mount: the token is stripped from the URL by captureToken so
  // it never lingers in the address bar, history or a screenshot.
  const [token] = useState(() => captureToken());
  const [route, setRoute] = useState<AppRoute>(() => parseRoute(window.location.hash));

  useEffect(() => {
    const onHashChange = (): void => setRoute(parseRoute(window.location.hash));
    window.addEventListener('hashchange', onHashChange);
    return () => {
      window.removeEventListener('hashchange', onHashChange);
    };
  }, []);

  const api = useMemo(
    () => new HearthClient(baseUrl === undefined ? { token } : { token, baseUrl }),
    [token, baseUrl],
  );

  const open = useCallback((id: string) => {
    window.location.hash = `#/s/${encodeURIComponent(id)}`;
  }, []);
  const back = useCallback(() => {
    window.location.hash = '#/';
  }, []);

  return (
    <div className="app">
      {token === '' && (
        <p className="banner banner-warn" role="status">
          no token: open this page with <code>?token=&lt;token&gt;</code> once, or rely on the
          server&apos;s HttpOnly cookie.
        </p>
      )}
      {route.name === 'list' ? (
        <SessionList api={api} onOpen={open} />
      ) : (
        <SessionView
          key={route.id}
          api={api}
          sessionId={route.id}
          token={token}
          {...(baseUrl === undefined ? {} : { baseUrl })}
          onBack={back}
        />
      )}
    </div>
  );
}