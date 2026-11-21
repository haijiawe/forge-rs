// Message composer. Enter sends, Shift+Enter is a newline.
//
// While a turn is running the box does not close: the same Enter steers that
// turn (§4.10) instead of opening the next one, and a Stop button asks it to
// end (§4.9). Two turns still cannot overlap on one session.
//
// Files: the picker gives the composer `File` objects, and attachments.ts turns
// them into the bytes POST /messages accepts. Nothing about encoding, kinds or
// size caps is decided here — this file only renders what that module decided,
// so a refusal is always a sentence the user can read rather than a file that
// vanished.
import { useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import type { OutgoingAttachment } from '../api/client';
import {
  addFiles,
  formatBytes,
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENTS,
  removeAttachment,
  toAttachments,
  type AttachmentRefusal,
  type DraftAttachment,
} from '../state/attachments';

export interface ComposerProps {
  /** True while the composer must not accept input (an approval is pending). */
  disabled: boolean;
  disabledReason?: string;
  /**
   * Resolves true when the message was accepted; the draft — text *and* files —
   * is cleared then, and only then.
   */
  onSend: (text: string, attachments: OutgoingAttachment[]) => Promise<boolean>;
  /** True while a turn is running: Enter steers it rather than sending. */
  running?: boolean;
  /** Resolves true when the text was appended to the running turn. */
  onSteer?: (text: string) => Promise<boolean>;
  /** Ask the running turn to stop; absent hides the Stop button. */
  onStop?: () => Promise<boolean>;
}

const PICKER_HINT = `图片与 .diff/.patch 文件，每个最大 ${formatBytes(MAX_ATTACHMENT_BYTES)}，每条消息 ${String(MAX_ATTACHMENTS)} 个`;

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
  const [attachments, setAttachments] = useState<DraftAttachment[]>([]);
  const [refusals, setRefusals] = useState<AttachmentRefusal[]>([]);
  const [reading, setReading] = useState(false);
  // The refs, not the state, are the guard. React commits discrete events
  // between events a browser delivers, so `sending` is enough for real input —
  // but two dispatches inside one act() (a double click in a test, or any
  // future caller that batches) both read the old state and both reach the
  // network. ApprovalDialog learned this first; the composer matches it.
  const sendingRef = useRef(false);
  const stoppingRef = useRef(false);
  // A pick is read asynchronously, so the handler must append to the list as it
  // is when the read finishes, not as it was when the handler closed over it;
  // two fast picks otherwise lose the first file.
  const attachmentsRef = useRef<DraftAttachment[]>([]);
  // Steering is a different verb with a different failure mode (409 when the
  // turn ended meanwhile), so it is only offered when the surface can do it.
  const steering = running && onSteer !== undefined;
  const blocked = disabled || sending;

  const commitAttachments = (next: DraftAttachment[]): void => {
    attachmentsRef.current = next;
    setAttachments(next);
  };

  const pick = async (event: ChangeEvent<HTMLInputElement>): Promise<void> => {
    const input = event.currentTarget;
    const picked = Array.from(input.files ?? []);
    // Clearing the input is what lets the same file be picked twice: a file
    // input only fires `change` when its value differs from the last one.
    input.value = '';
    setReading(true);
    try {
      const result = await addFiles(picked, attachmentsRef.current);
      if (result.added.length > 0) {
        commitAttachments([...attachmentsRef.current, ...result.added]);
      }
      // Refusals replace, they do not accumulate: the alert describes the pick
      // that just happened, and the next pick (or a successful send) clears it.
      setRefusals(result.refused);
    } finally {
      setReading(false);
    }
  };

  const submit = async (): Promise<void> => {
    if (sendingRef.current || blocked || reading || draft.trim() === '') return;
    const files = toAttachments(attachmentsRef.current);
    // A steer has no attachments on the wire (SteerRequest carries only text),
    // so steering with a file attached would drop it without a word. Refuse
    // instead: a sentence is recoverable, a lost screenshot is not.
    if (steering && files.length > 0) {
      setRefusals([
        {
          name: null,
          reason:
            '转交时不能附带文件：正在运行的回合只接受文本。等这一回合结束后，再把它作为消息发送。',
        },
      ]);
      return;
    }
    sendingRef.current = true;
    setSending(true);
    try {
      const accepted = steering ? await onSteer(draft) : await onSend(draft, files);
      if (accepted) {
        // Cleared on acceptance only. A failed send keeps the text and the
        // files: the store reports the failure in its own banner, and a file
        // the user picked from disk cannot be recovered by retyping.
        setDraft('');
        commitAttachments([]);
        setRefusals([]);
      }
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
    ? (disabledReason ?? '代理正在运行…')
    : steering
      ? '引导正在运行的回合 — 按 Enter 在步骤之间插入'
      : '给代理发消息 — Enter 发送，Shift+Enter 换行';

  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      {refusals.length > 0 && (
        <div className="composer-strip composer-refusals" role="alert">
          <ul className="composer-strip">
            {refusals.map((refusal, index) => (
              // A refusal has no id of its own and two files may share a name,
              // so the index is the only stable part of the key here.
              <li key={`${refusal.name ?? 'selection'}-${String(index)}`}>{refusal.reason}</li>
            ))}
          </ul>
          <button
            type="button"
            className="link-button"
            onClick={() => {
              setRefusals([]);
            }}
          >
            关闭
          </button>
        </div>
      )}

      {attachments.length > 0 && (
        <ul className="composer-strip composer-attachments">
          {attachments.map((attachment) => (
            <li key={attachment.id} className="composer-attachment">
              <span className="mono composer-attachment-name" title={attachment.name}>
                {attachment.name}
              </span>{' '}
              <span className="muted">
                {attachment.kind} · {formatBytes(attachment.bytes)}
              </span>{' '}
              <button
                type="button"
                className="link-button"
                aria-label={`移除 ${attachment.name}`}
                disabled={blocked}
                onClick={() => {
                  commitAttachments(removeAttachment(attachmentsRef.current, attachment.id));
                }}
              >
                移除
              </button>
            </li>
          ))}
        </ul>
      )}

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
      {/* A real file input, not a hidden one behind a styled label: a
          display:none input cannot be tabbed to, and a mouse-only affordance is
          the one thing a keyboard user cannot work around. It stays in the tab
          order and opens with Enter/Space on its own. */}
      <input
        className="composer-attach"
        type="file"
        multiple
        accept="image/*,.diff,.patch"
        aria-label="添加图片或 .diff/.patch 文件"
        title={PICKER_HINT}
        disabled={blocked || reading}
        onChange={(event) => {
          void pick(event);
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
          {stopping ? '正在停止…' : '停止'}
        </button>
      )}
      <button type="submit" className="button button-primary" disabled={blocked || draft.trim() === ''}>
        {sending ? '正在发送…' : steering ? '引导' : '发送'}
      </button>
    </form>
  );
}