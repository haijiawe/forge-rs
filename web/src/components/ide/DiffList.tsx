// The diff list: the centre pane's second mode, switched on from the toolbar.
//
// The contract makes `files: []` mean two different things — not a git
// repository, or a clean tree — and it does not say which. So the empty state
// names both possibilities instead of picking one: a sentence that claims
// "no changes" when there is no repository at all would be a lie the reader
// cannot check.
import type { DiffEntry } from '../../api/workbench';

export interface DiffListProps {
  files: readonly DiffEntry[] | null;
  loading: boolean;
  error: string | null;
  onOpenFile: (path: string) => void;
  onRetry: () => void;
}

export function DiffList({ files, loading, error, onOpenFile, onRetry }: DiffListProps) {
  return (
    <div className="wb-diff">
      <div className="wb-diff-head">
        <span>working tree</span>
        {files !== null && <span className="wb-muted">{files.length} file(s)</span>}
      </div>
      {loading && <p className="wb-empty">reading the diff…</p>}
      {error !== null && (
        <div className="wb-empty wb-error">
          <p>{error}</p>
          <button type="button" className="button button-ghost" onClick={onRetry}>
            try again
          </button>
        </div>
      )}
      {!loading && error === null && files === null && (
        <p className="wb-empty">the diff has not been read yet.</p>
      )}
      {!loading && error === null && files !== null && files.length === 0 && (
        <p className="wb-empty">
          the session reported no changed files. That answer means either the workspace is not a git
          repository or its working tree is clean — the endpoint does not distinguish the two, so
          neither does this list.
        </p>
      )}
      {!loading && error === null && files !== null && files.length > 0 && (
        <ul className="wb-diff-list">
          {files.map((entry) => (
            <li key={entry.path}>
              <button
                type="button"
                className="wb-diff-row"
                title={entry.path}
                onClick={() => {
                  onOpenFile(entry.path);
                }}
              >
                <span className="wb-diff-status">{entry.status}</span>
                <span className="wb-diff-path">{entry.path}</span>
                <span className="wb-diff-counts">
                  <span className="wb-good">+{entry.additions}</span>{' '}
                  <span className="wb-bad">-{entry.deletions}</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}