// The tab strip above the editor: one button per open file, plus its close
// cross.
//
// The strip is the only place the workbench shows which files are open, so a
// tab that failed to read still appears here — with its name — and the pane
// below says what went wrong. Dropping the tab would hide the failure.
import type { FileTab } from '../../state/workbench';

export interface TabsProps {
  open: readonly FileTab[];
  activePath: string | null;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
}

export function Tabs({ open, activePath, onSelect, onClose }: TabsProps) {
  if (open.length === 0) {
    return (
      <div className="wb-tabs wb-tabs-empty">
        <span className="wb-muted">没有打开的文件</span>
      </div>
    );
  }
  return (
    <div className="wb-tabs" role="tablist" aria-label="已打开的文件">
      {open.map((tab) => {
        const active = tab.path === activePath;
        const name = tab.path.slice(tab.path.lastIndexOf('/') + 1);
        return (
          <div key={tab.path} className={active ? 'wb-tab wb-tab-active' : 'wb-tab'}>
            <button
              type="button"
              role="tab"
              aria-selected={active}
              className="wb-tab-label"
              title={tab.error ?? tab.path}
              onClick={() => {
                onSelect(tab.path);
              }}
            >
              {name}
              {tab.loading && <span className="wb-tab-flag"> …</span>}
              {tab.error != null && (
                <span className="wb-tab-flag wb-error" aria-label="读取失败">
                  {' '}
                  !
                </span>
              )}
            </button>
            <button
              type="button"
              className="wb-tab-close"
              aria-label={`关闭 ${tab.path}`}
              onClick={() => {
                onClose(tab.path);
              }}
            >
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}