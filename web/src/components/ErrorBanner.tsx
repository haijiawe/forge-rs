// 一条错误横幅：先说结论，把栈收到第二行以后。
//
// 失败信息常常是一整段栈。全部摊在转录顶上，会把输入框顶出屏幕；只显示第一行，又
// 把唯一能定位问题的那一帧（往往正是第二行）藏掉了。所以默认露两行，其余收在
// 「展开」后面，由读者决定要不要看。
//
// 这个「两行 + 可展开」的做法抄自 UI-TARS-desktop 的 ErrorMessage
// （apps/ui-tars/src/renderer/src/components/RunMessages/Messages.tsx，Apache-2.0）：
// 它也是 MAX_LINE = 2 加一个可展开的栈。只抄骨架——外壳仍是我们自己的 .banner，
// 因为这里的样式是手写 CSS，没有 Tailwind。
import { useState } from 'react';
import type { ReactElement } from 'react';

/** How many lines a reader sees before asking for the rest. */
const COLLAPSED_LINES = 2;

export interface ErrorBannerProps {
  /** One short sentence naming what failed; the detail is appended to it. */
  title?: string;
  /** The failure itself: the API's message, a stack, whatever the server said. */
  detail: string;
  /** The button that acts on this failure. Omitted when there is nothing to do. */
  action?: { label: string; onClick: () => void };
}

export function ErrorBanner({ title, detail, action }: ErrorBannerProps): ReactElement {
  const [open, setOpen] = useState(false);
  // A trailing newline is not an empty last line; splitting without this would
  // count one line too many and offer a "show more" that shows nothing.
  const lines = detail.replace(/\n$/, '').split('\n');
  const hidden = lines.length > COLLAPSED_LINES;
  const shown = (open || !hidden ? lines : lines.slice(0, COLLAPSED_LINES)).join('\n');
  return (
    <div className="banner banner-error" role="alert">
      {/* The title is concatenated rather than put in its own element: the
          sentences read as one ("读取这个会话失败：unauthorized"), and a
          separate span would put a whitespace node between them. */}
      <p className="banner-detail">{title === undefined ? shown : `${title}${shown}`}</p>
      <div className="banner-actions">
        {hidden && (
          <button
            type="button"
            className="link-button"
            aria-expanded={open}
            onClick={() => {
              setOpen((on) => !on);
            }}
          >
            {open ? '收起' : `展开其余 ${lines.length - COLLAPSED_LINES} 行`}
          </button>
        )}
        {action !== undefined && (
          <button type="button" className="link-button" onClick={action.onClick}>
            {action.label}
          </button>
        )}
      </div>
    </div>
  );
}
