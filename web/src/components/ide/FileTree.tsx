// The file tree: the workspace root's listing, with directories that open in
// place.
//
// It draws exactly the three states the store keeps and never guesses at a
// fourth: a directory with no answer says so, an empty directory says it is
// empty, and a failed read shows the server's sentence. An empty root therefore
// reads as "the workspace has no files", not as silence.
//
// Sorting is the server's job (directories first, then by name) because the
// contract says so; re-sorting here would hide a server that got it wrong.
import type { DirListing, FileEntry } from '../../api/workbench';
import type { DirState } from '../../state/workbench';

export interface FileTreeProps {
  dirs: Readonly<Record<string, DirState>>;
  expanded: readonly string[];
  activePath: string | null;
  onToggleDir: (path: string) => void;
  onOpenFile: (path: string) => void;
}

/** `dir` + `name`, with the root's empty path handled in one place. */
export function joinPath(dir: string, name: string): string {
  return dir === '' ? name : `${dir}/${name}`;
}

const INDENT_PX = 12;

function Level({
  dir,
  depth,
  dirs,
  expanded,
  activePath,
  onToggleDir,
  onOpenFile,
}: FileTreeProps & { dir: string; depth: number }) {
  const state = dirs[dir];
  const listing: DirListing | null = state?.listing ?? null;
  if (listing === null) return null;
  if (listing.entries.length === 0) {
    return (
      <p className="wb-empty wb-empty-nested" style={{ paddingLeft: `${String(8 + depth * INDENT_PX)}px` }}>
        这个目录是空的
      </p>
    );
  }
  return (
    <ul className="wb-tree-list">
      {listing.entries.map((entry: FileEntry) => {
        const path = joinPath(dir, entry.name);
        if (entry.kind === 'dir') {
          const isOpen = expanded.includes(path);
          const child = dirs[path];
          return (
            <li key={path}>
              <button
                type="button"
                className="wb-row wb-row-dir"
                aria-expanded={isOpen}
                title={path}
                style={{ paddingLeft: `${String(6 + depth * INDENT_PX)}px` }}
                onClick={() => {
                  onToggleDir(path);
                }}
              >
                <span className="wb-caret" aria-hidden="true">
                  {isOpen ? '▾' : '▸'}
                </span>
                <span className="wb-name">{entry.name}</span>
              </button>
              {isOpen && child?.loading === true && (
                <p className="wb-empty wb-empty-nested" style={{ paddingLeft: `${String(8 + (depth + 1) * INDENT_PX)}px` }}>
                  正在读取 {path}…
                </p>
              )}
              {isOpen && child?.error != null && (
                <p
                  className="wb-empty wb-empty-nested wb-error"
                  style={{ paddingLeft: `${String(8 + (depth + 1) * INDENT_PX)}px` }}
                >
                  {child.error}
                </p>
              )}
              {isOpen && child?.error == null && (
                <Level
                  dir={path}
                  depth={depth + 1}
                  dirs={dirs}
                  expanded={expanded}
                  activePath={activePath}
                  onToggleDir={onToggleDir}
                  onOpenFile={onOpenFile}
                />
              )}
            </li>
          );
        }
        const active = path === activePath;
        return (
          <li key={path}>
            <button
              type="button"
              className={active ? 'wb-row wb-row-file wb-row-active' : 'wb-row wb-row-file'}
              aria-current={active ? 'true' : undefined}
              title={entry.size === undefined ? path : `${path} · ${String(entry.size)} 字节`}
              style={{ paddingLeft: `${String(6 + depth * INDENT_PX + INDENT_PX)}px` }}
              onClick={() => {
                onOpenFile(path);
              }}
            >
              <span className="wb-name">{entry.name}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function FileTree({ dirs, expanded, activePath, onToggleDir, onOpenFile }: FileTreeProps) {
  const root = dirs[''];
  const empty = root?.listing != null && root.listing.entries.length === 0;
  return (
    <nav className="wb-tree" aria-label="工作区文件">
      <h2 className="wb-pane-head">文件</h2>
      {root === undefined && <p className="wb-empty">工作区列表还没有被请求过。</p>}
      {root?.loading === true && <p className="wb-empty">正在读取工作区…</p>}
      {root?.error != null && <p className="wb-empty wb-error">{root.error}</p>}
      {empty && (
        <p className="wb-empty">
          该会话的工作区根目录没有列出任何条目。这个桥接只列出工作区之内的文件，
          所以根目录为空就意味着目录确实是空的 — 如果是读取失败，这里显示的会是错误信息。
        </p>
      )}
      {root?.listing != null && !empty && (
        <Level
          dir=""
          depth={0}
          dirs={dirs}
          expanded={expanded}
          activePath={activePath}
          onToggleDir={onToggleDir}
          onOpenFile={onOpenFile}
        />
      )}
    </nav>
  );
}