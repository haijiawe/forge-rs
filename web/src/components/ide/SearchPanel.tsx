// The search box and its results, drawn in the left column in place of the tree.
//
// Two decisions worth stating, because both are about not lying:
//
//   * "没有匹配" is only said after the server named how many files it read. A
//     search that stopped at its cap and a search that walked the whole tree
//     both end with an empty list, and they are different facts — the first one
//     gets "结果不完整" appended, the second one does not.
//   * The box keeps its own text while the store keeps the answer. A keystroke
//     must not re-run a search across a workspace (that is a disk walk), so the
//     query is sent on Enter or on the button, never on change.
import { useState } from 'react';
import type { FormEvent } from 'react';

import type { SearchState } from '../../state/workbench';

export interface SearchPanelProps {
  search: SearchState | null;
  onSearch: (query: string) => void;
  onClear: () => void;
  onOpenHit: (path: string, line: number) => void;
}

export function SearchPanel({ search, onSearch, onClear, onOpenHit }: SearchPanelProps) {
  const [draft, setDraft] = useState('');

  const submit = (event: FormEvent): void => {
    event.preventDefault();
    const query = draft.trim();
    if (query === '') {
      onClear();
      return;
    }
    onSearch(query);
  };

  return (
    <div className="wb-search">
      <form className="wb-search-form" onSubmit={submit} role="search">
        <input
          className="wb-search-input"
          type="search"
          value={draft}
          placeholder="在工作区里搜索"
          aria-label="在工作区里搜索"
          spellCheck={false}
          onChange={(event) => {
            setDraft(event.target.value);
          }}
        />
        <button type="submit" className="wb-search-go">
          搜索
        </button>
        {search !== null && (
          <button
            type="button"
            className="wb-search-clear"
            onClick={() => {
              setDraft('');
              onClear();
            }}
          >
            清除
          </button>
        )}
      </form>
      {search !== null && <SearchResults search={search} onOpenHit={onOpenHit} />}
    </div>
  );
}

function SearchResults({
  search,
  onOpenHit,
}: {
  search: SearchState;
  onOpenHit: (path: string, line: number) => void;
}) {
  if (search.loading) {
    return <p className="wb-empty">正在搜索 {search.query} …</p>;
  }
  if (search.error !== null) {
    return <p className="wb-empty wb-error">{search.error}</p>;
  }
  const answer = search.answer;
  if (answer === null) return null;
  if (answer.hits.length === 0) {
    return (
      <p className="wb-empty">
        没有匹配「{answer.query}」，扫过 {answer.files} 个文件。
        {answer.truncated ? '结果不完整：搜索在读完整个工作区之前就到达了上限。' : ''}
      </p>
    );
  }
  return (
    <div className="wb-search-results">
      <p className="wb-search-count">
        {answer.hits.length} 条匹配，扫过 {answer.files} 个文件
        {answer.truncated ? '（结果不完整：已到上限）' : ''}
      </p>
      <ul className="wb-search-list">
        {answer.hits.map((hit) => (
          <li key={`${hit.path}:${String(hit.line)}:${String(hit.column)}`}>
            <button
              type="button"
              className="wb-row wb-search-hit"
              title={`${hit.path}:${String(hit.line)}:${String(hit.column)}`}
              onClick={() => {
                onOpenHit(hit.path, hit.line);
              }}
            >
              <span className="wb-search-where">
                {hit.path}
                <span className="wb-search-line">:{String(hit.line)}</span>
              </span>
              <span className="wb-search-text">{hit.text}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}