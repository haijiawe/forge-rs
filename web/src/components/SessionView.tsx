// The session screen: transcript + composer + approval + status bar.
//
// The store owns all state; this component subscribes with
// useSyncExternalStore and re-renders once per published frame (the store
// batches to requestAnimationFrame, see state/sessionStore.ts).
import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import type { HearthApi, OutgoingAttachment } from '../api/client';
import type { CapabilityReport } from '../api/capabilities';
import type { Goal } from '../api/goal';
import type { ApprovalDecision } from '../api/types';
import type { SubagentTree } from '../api/subagents';
import { SessionStore } from '../state/sessionStore';
import { ApprovalDialog } from './ApprovalDialog';
import { CapabilityPanel } from './CapabilityPanel';
import { Composer } from './Composer';
import { GoalPill } from './GoalPill';
import { StatusBar } from './StatusBar';
import { SubagentPanel } from './SubagentPanel';
import { Transcript } from './Transcript';
import { Workbench, useWideLayout } from './ide/Workbench';

export interface SessionViewProps {
  api: HearthApi;
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
        setReportError(error instanceof Error ? error.message : '能力信息不可用');
      },
    );
    return () => {
      live = false;
    };
  }, [api]);

  // The goal is three lines in the log, and only a turn writes them, so the read
  // is rare and cheap: once when the stream is settled and again after every
  // finished turn. `settled` is that trigger -- it is null while a turn is in
  // flight, and the effect re-runs each time it turns from null back into a
  // status word.
  const settled =
    state.status === 'idle' || state.status === 'ended' ? state.status : null;
  const [goal, setGoal] = useState<Goal | null>(null);
  const [goalError, setGoalError] = useState<string | null>(null);

  // The subagent tree moves exactly when a turn ends -- a child is started by a
  // `subagent` tool call and settles inside the turn that started it, or a
  // later one -- so it is read on the same trigger as the goal. Reading it per
  // frame would re-walk every child log for numbers that cannot have changed.
  const [subagents, setSubagents] = useState<SubagentTree | null>(null);
  const [subagentError, setSubagentError] = useState<string | null>(null);
  useEffect(() => {
    if (settled === null) return;
    let live = true;
    api.subagents(sessionId).then(
      (value) => {
        if (!live) return;
        setSubagents(value);
        setSubagentError(null);
      },
      (error: unknown) => {
        if (!live) return;
        setSubagentError(
          error instanceof Error ? error.message : '子代理信息不可用',
        );
      },
    );
    return () => {
      live = false;
    };
  }, [api, sessionId, settled]);
  useEffect(() => {
    if (settled === null) return;
    let live = true;
    let retry: ReturnType<typeof setTimeout> | null = null;
    // One retry before the failure is shown. The first read can race the load of
    // the document itself -- in the desktop shell the window navigates into this
    // session while the page is still settling -- and a red sentence in the
    // toolbar for a request that succeeds 400 ms later is a lie the reader has
    // no way to check.
    const read = (attempt: number): void => {
      api.getGoal(sessionId).then(
        (value) => {
          if (!live) return;
          setGoal(value);
          setGoalError(null);
        },
        // A goal that cannot be read must not take the turn controls with it:
        // the session still works, so the pill carries the failure and nothing
        // else changes.
        (error: unknown) => {
          if (!live) return;
          if (attempt === 0) {
            retry = setTimeout(() => {
              read(1);
            }, 400);
            return;
          }
          setGoalError(error instanceof Error ? error.message : '目标信息不可用');
        },
      );
    };
    read(0);
    return () => {
      live = false;
      if (retry !== null) clearTimeout(retry);
    };
  }, [api, sessionId, settled]);

  const send = useCallback(
    (text: string, attachments: readonly OutgoingAttachment[]) =>
      store.sendMessage(text, attachments),
    [store],
  );
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
  const blocksLabel = `${String(state.blocks.length)} 个块已从模型中隐藏`;

  // One screen, two shapes. Everything below the fold is built once and placed
  // in either layout: the workbench wraps the same transcript and composer in a
  // three-column shell, and a window narrower than 900px keeps exactly the
  // single column this screen has always been. `useWideLayout` answers false
  // where matchMedia does not exist, so nothing degrades into a layout it was
  // never tested in.
  const wide = useWideLayout();

  const head = (
    <header className="session-head">
      <button type="button" className="button button-ghost" onClick={onBack}>
        ← 会话列表
      </button>
      <div className="session-title">
        <span className="mono">{state.session?.id ?? sessionId}</span>
        {state.session !== null && (
          <span className="mono muted" title={state.session.workspace}>
            {state.session.workspace}
          </span>
        )}
      </div>
      <GoalPill goal={goal} error={goalError} />
      {state.blocks.length > 0 && (
        <span
          className="session-compressed"
          title={`块 id：${state.blocks.map((block) => block.blockId).join(', ')}`}
        >
          {blocksLabel}
        </span>
      )}
    </header>
  );

  const agent = (
    <>
      {state.actionError !== null && (
        <p className="banner banner-error" role="alert">
          {state.actionError}
          <button type="button" className="link-button" onClick={dismiss}>
            关闭
          </button>
        </p>
      )}

      {state.needsResync && (
        <p className="banner banner-warn">正在从事件日志重新同步对话记录…</p>
      )}

      <Transcript rows={state.rows} rowKeys={state.rowKeys} emptyHint="正在等待第一个事件…" />

      <CapabilityPanel names={state.toolNames} report={report} reportError={reportError} />

      <SubagentPanel tree={subagents} error={subagentError} />

      {state.pendingApproval !== null && (
        <ApprovalDialog pending={state.pendingApproval} onDecide={decide} />
      )}

      <Composer
        disabled={state.status === 'awaiting_approval'}
        disabledReason="正在等待审批…"
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
    </>
  );

  if (wide) {
    return (
      <Workbench
        sessionId={sessionId}
        head={head}
        agent={agent}
        connecting={state.connecting}
        needsResync={state.needsResync}
        turnChanges={state.turnChanges}
      />
    );
  }

  return (
    <main className="screen screen-session">
      {head}
      {agent}
    </main>
  );
}