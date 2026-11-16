// Message composer. Enter sends, Shift+Enter is a newline.
//
// While a turn is running the box does not close: the same Enter steers that
// turn (§4.10) instead of opening the next one, and a Stop button asks it to
// end (§4.9). Two turns still cannot overlap on one session.
import { useRef, useState } from 'react';

export interface ComposerProps {
  /** True while the composer must not accept input (an approval is pending). */
  disabled: boolean;
  disabledReason?: string;
  /** Resolves true when the message was accepted; the draft is cleared then. */
  onSend: (text: string) => Promise<boolean>;
  /** True while a turn is running: Enter steers it rather than sending. */
  running?: boolean;
  /** Resolves true when the text was appended to the running turn. */
  onSteer?: (text: string) => Promise<boolean>;
  /** Ask the running turn to stop; absent hides the Stop button. */
  onStop?: () => Promise<boolean>;
}

export function Composer({
  disabled,
  disabledReason,
  onSend,
  running = false,
  onSteer,
  onStop,
}: ComposerProps) {
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [stopping, setStopping] = useState(false);
  // The refs, not the state, are the guard. React commits discrete events
  // between events a browser delivers, so `sending` is enough for real input —
  // but two dispatches inside one act() (a double click in a test, or any
  // future caller that batches) both read the old state and both reach the
  // network. ApprovalDialog learned this first; the composer matches it.
  const sendingRef = useRef(false);
  const stoppingRef = useRef(false);
  // Steering is a different verb with a different failure mode (409 when the
  // turn ended meanwhile), so it is only offered when the surface can do it.
  const steering = running && onSteer !== undefined;
  const blocked = disabled || sending;

  const submit = async (): Promise<void> => {
    if (sendingRef.current || blocked || draft.trim() === '') return;
    sendingRef.current = true;
    setSending(true);
    try {
      const accepted = steering ? await onSteer(draft) : await onSend(draft);
      if (accepted) setDraft('');
    } catch {
      // A handler that throws instead of resolving false is a bug in the
      // caller, but swallowing it here keeps the composer usable and the draft
      // intact; the surface that owns the error banner reports the failure.
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  const stop = async (): Promise<void> => {
    if (onStop === undefined || stoppingRef.current) return;
    stoppingRef.current = true;
    setStopping(true);
    try {
      await onStop();
    } catch {
      // Same reasoning as submit(): the button must come back.
    } finally {
      stoppingRef.current = false;
      setStopping(false);
    }
  };

  const placeholder = disabled
    ? (disabledReason ?? 'the agent is running…')
    : steering
      ? 'Steer the running turn — Enter injects it between steps'
      : 'Message the agent — Enter sends, Shift+Enter adds a line';

  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <textarea
        className="composer-input"
        value={draft}
        rows={2}
        placeholder={placeholder}
        disabled={blocked}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
            event.preventDefault();
            void submit();
          }
        }}
      />
      {running && onStop !== undefined && (
        <button
          type="button"
          className="button button-ghost"
          disabled={stopping}
          onClick={() => {
            void stop();
          }}
        >
          {stopping ? 'stopping…' : 'Stop'}
        </button>
      )}
      <button type="submit" className="button button-primary" disabled={blocked || draft.trim() === ''}>
        {sending ? 'sending…' : steering ? 'Steer' : 'Send'}
      </button>
    </form>
  );
}