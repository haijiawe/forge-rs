// The session screen: transcript + composer + approval + status bar.
//
// The store owns all state; this component subscribes with
// useSyncExternalStore and re-renders once per published frame (the store
// batches to requestAnimationFrame, see state/sessionStore.ts).
import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import type { ForgeApi } from '../api/client';
import type { CapabilityReport } from '../api/capabilities';
import type { ApprovalDecision } from '../api/types';
import { SessionStore } from '../state/sessionStore';
import { ApprovalDialog } from './ApprovalDialog';
import { CapabilityPanel } from './CapabilityPanel';
import { Composer } from './Composer';
import { StatusBar } from './StatusBar';
import { Transcript } from './Transcript';

export interface SessionViewProps {
  api: ForgeApi;
  sessionId: string;
  token: string;
  baseUrl?: string;
  onBack: () => void;
}

export function SessionView({ api, sessionId, token, baseUrl, onBack }: SessionViewProps) {
  const store = useMemo(() => {
    const options = {
      api,
      sessionId,
      token,
      ...(baseUrl === undefined ? {} : { baseUrl }),
    };
    return new SessionStore(options);
  }, [api, sessionId, token, baseUrl]);

  useEffect(() => {
    store.start();
    return () => {
      store.stop();
    };
  }, [store]);

  const state = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);

  // One GET at mount. Configuration does not change while the process runs --
  // plugins are loaded once, before the server binds -- so there is nothing to
  // poll for, and a failure here must leave the pane quiet rather than break
  // the session screen.
  const [report, setReport] = useState<CapabilityReport | null>(null);
  const [reportError, setReportError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api.capabilities().then(
      (value) => {
        if (live) setReport(value);
      },
      (error: unknown) => {
        if (!live) return;
        setReportError(error instanceof Error ? error.message : 'capabilities unavailable');
      },
    );
    return () => {
      live = false;
    };
  }, [api]);

  const send = useCallback((text: string) => store.sendMessage(text), [store]);
  const steer = useCallback((text: string) => store.steerTurn(text), [store]);
  const stop = useCallback(() => store.cancelTurn(), [store]);
  const decide = useCallback(
    async (decision: ApprovalDecision, note?: string): Promise<void> => {
      await store.decide(decision, note);
    },
    [store],
  );
  const dismiss = useCallback(() => store.clearActionError(), [store]);

  const running = state.status === 'running';
  // The turn is open while it waits for an approval too, so stopping stays
  // available there; only input closes, because the dialog owns the decision.
  const turnActive = running || state.status === 'awaiting_approval';
  // Counted from the projection, not stored twice: `blocks` is what the log
  // wrote, and the marks in `rows` are what those blocks hid.
  const blocksLabel = `${String(state.blocks.length)} block${state.blocks.length === 1 ? '' : 's'} hidden from the model`;

  return (
    <main className="screen screen-session">
      <header className="session-head">
        <button type="button" className="button button-ghost" onClick={onBack}>
          ← sessions
        </button>
        <div className="session-title">
          <span className="mono">{state.session?.id ?? sessionId}</span>
          {state.session !== null && (
            <span className="mono muted" title={state.session.workspace}>
              {state.session.workspace}
            </span>
          )}
        </div>
        {state.blocks.length > 0 && (
          <span
            className="session-compressed"
            title={`block ids: ${state.blocks.map((block) => block.blockId).join(', ')}`}
          >
            {blocksLabel}
          </span>
        )}
      </header>

      {state.actionError !== null && (
        <p className="banner banner-error" role="alert">
          {state.actionError}
          <button type="button" className="link-button" onClick={dismiss}>
            dismiss
          </button>
        </p>
      )}

      {state.needsResync && (
        <p className="banner banner-warn">resyncing transcript from the event log…</p>
      )}

      <Transcript rows={state.rows} rowKeys={state.rowKeys} emptyHint="waiting for the first event…" />

      <CapabilityPanel names={state.toolNames} report={report} reportError={reportError} />

      {state.pendingApproval !== null && (
        <ApprovalDialog pending={state.pendingApproval} onDecide={decide} />
      )}

      <Composer
        disabled={state.status === 'awaiting_approval'}
        disabledReason="waiting for an approval…"
        onSend={send}
        running={turnActive}
        onSteer={steer}
        onStop={stop}
      />

      <StatusBar
        session={state.session}
        status={state.status}
        sessionId={state.session?.id ?? sessionId}
        lastSeq={state.lastSeq}
        connecting={state.connecting}
        needsResync={state.needsResync}
        streamError={state.streamError}
      />
    </main>
  );
}