// The one module that touches Monaco. Everything about *how* the editor is
// packaged lives here so the component stays about files and buffers.
//
// Three decisions, each forced by something measured or by the product:
//
//   * The full `monaco-editor` ESM entry, not `editor.api`. `editor.api` ships
//     the editor with none of its contributions — no find widget, no bracket
//     matching, no context menu — and rebuilding that list by hand means hard-
//     coding internal module paths that move between releases. The pane is an
//     IDE's editor, so it gets the whole thing.
//
//   * Workers by Vite's `?worker` import, never the CDN. Monaco's default loader
//     fetches its worker from a public URL; the desktop shell has no network
//     guarantee and the web UI is served from loopback. A worker fetched over
//     the internet is a dependency on somebody else's uptime, and in the shell
//     it is simply a broken editor. `?worker` bundles it into the build.
//
//   * A capability probe before importing anything. jsdom has no canvas, so the
//     test suite would load several megabytes of editor to then fail inside it.
//     Probing a real canvas is a statement about the environment rather than
//     about the test runner, so the fallback is honest in both. The probe itself
//     lives in `canvas.ts`, which is imported statically by the component so it
//     can decide whether to load *this* module at all.
//
// The languages are a list, not `basic-languages/monaco.contribution`: that
// bundle carries ~60 grammars for a coding agent that edits source files. The
// list below is what a workspace of this kind actually contains, and a file
// with an extension outside it is shown as plain text — visible, just not
// coloured.

import * as monaco from 'monaco-editor';

// Grammars. Each import registers one language with the same mechanism the
// bundle would have used.
import 'monaco-editor/esm/vs/basic-languages/rust/rust.contribution';
import 'monaco-editor/esm/vs/basic-languages/typescript/typescript.contribution';
import 'monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution';
import 'monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution';
import 'monaco-editor/esm/vs/basic-languages/python/python.contribution';
import 'monaco-editor/esm/vs/basic-languages/go/go.contribution';
import 'monaco-editor/esm/vs/basic-languages/shell/shell.contribution';
import 'monaco-editor/esm/vs/basic-languages/css/css.contribution';
import 'monaco-editor/esm/vs/basic-languages/html/html.contribution';
import 'monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution';
import 'monaco-editor/esm/vs/basic-languages/ini/ini.contribution';
import 'monaco-editor/esm/vs/basic-languages/sql/sql.contribution';
import 'monaco-editor/esm/vs/basic-languages/xml/xml.contribution';
import 'monaco-editor/esm/vs/basic-languages/dockerfile/dockerfile.contribution';

// The worker. Tokenisation and the model live in it; without one, Monaco logs
// an error per keystroke and falls back to plain text.
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';

/**
 * True when this environment can host Monaco: it needs a layout engine and a
 * 2D canvas for text measurement, and jsdom implements neither.
 *
 * Re-exported from `canvas.ts` for callers that already have Monaco loaded; the
 * component uses the static module so it can decide before importing this one.
 */
export { canvasAvailable as canRunMonaco } from './canvas';

let configured = false;

/** Install the worker factory once; a second call is a no-op. */
export function configureMonaco(): void {
  if (configured) return;
  configured = true;
  (self as unknown as { MonacoEnvironment?: monaco.Environment }).MonacoEnvironment = {
    getWorker: () => new EditorWorker(),
  };
  registerHandWrittenLanguages();
}

/**
 * The two languages Monaco ships everywhere except its ESM grammar folder.
 *
 * `json` is not in `esm/vs/basic-languages/` because it is a *language service*
 * there — importing it drags in a JSON worker whose absence turns every model
 * creation into a console error. `diff` has no grammar at all. Both are cheap to
 * tokenise here, and a tokeniser with no worker is exactly the right size for a
 * pane that only needs colour.
 *
 * Registered only when the id is free, so an environment that does provide them
 * keeps its own.
 */
function registerHandWrittenLanguages(): void {
  const known = new Set(monaco.languages.getLanguages().map((entry) => entry.id));

  if (!known.has('json')) {
    monaco.languages.register({ id: 'json', extensions: ['.json', '.jsonc'] });
    monaco.languages.setMonarchTokensProvider('json', {
      tokenizer: {
        root: [
          [/\s+/, 'white'],
          [/"[^"\\]*(\\.[^"\\]*)*"/, 'string'],
          [/-?\d+(\.\d+)?([eE][-+]?\d+)?/, 'number'],
          [/\b(true|false|null)\b/, 'keyword'],
          [/[{}[\]]/, 'delimiter.bracket'],
          [/[:,]/, 'delimiter'],
        ],
      },
    });
  }

  if (!known.has('diff')) {
    monaco.languages.register({ id: 'diff', extensions: ['.diff', '.patch'] });
    monaco.languages.setMonarchTokensProvider('diff', {
      tokenizer: {
        root: [
          [/^@@.*$/, 'keyword'],
          [/^\+.*$/, 'string'],
          [/^-.*$/, 'invalid'],
          [/^diff .*$/, 'type'],
          [/^index .*$/, 'comment'],
        ],
      },
    });
  }
}

/** Extensions we colour; everything else is plain text, deliberately. */
const LANGUAGES: Readonly<Record<string, string>> = {
  rs: 'rust',
  ts: 'typescript',
  tsx: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  json: 'json',
  jsonc: 'json',
  md: 'markdown',
  markdown: 'markdown',
  py: 'python',
  go: 'go',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  ps1: 'powershell',
  css: 'css',
  html: 'html',
  htm: 'html',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'ini',
  ini: 'ini',
  conf: 'ini',
  sql: 'sql',
  xml: 'xml',
  svg: 'xml',
  dockerfile: 'dockerfile',
  diff: 'diff',
  patch: 'diff',
};

/**
 * The Monaco language id for a path, or `plaintext`.
 *
 * Matched on the last extension only. A `Dockerfile` with no extension is
 * caught by name, because that is the one extensionless file a workspace of
 * this kind reliably has.
 */
export function languageFor(path: string): string {
  const name = path.slice(path.lastIndexOf('/') + 1);
  if (name.toLowerCase() === 'dockerfile') return 'dockerfile';
  const dot = name.lastIndexOf('.');
  if (dot <= 0 || dot === name.length - 1) return 'plaintext';
  const extension = name.slice(dot + 1).toLowerCase();
  return LANGUAGES[extension] ?? 'plaintext';
}

/**
 * The editor theme, matching the shell's own palette.
 *
 * Defined here rather than in CSS because Monaco draws on a canvas and reads
 * colours from its theme registry. `vs-dark` is close but its background is
 * lighter than this UI's, and a rectangle of somebody else's dark grey inside
 * the pane reads as a rendering bug.
 */
const THEME = 'hearth-dark';

export function applyTheme(): void {
  monaco.editor.defineTheme(THEME, {
    base: 'vs-dark',
    inherit: true,
    rules: [],
    colors: {
      'editor.background': '#0f1115',
      'editor.foreground': '#d8dbe2',
      'editorLineNumber.foreground': '#4a5160',
      'editorLineNumber.activeForeground': '#9aa3b2',
      'editor.selectionBackground': '#2d3a52',
      'editor.lineHighlightBackground': '#171a20',
      'editorCursor.foreground': '#ff8a3d',
      'editorIndentGuide.background1': '#20242c',
    },
  });
}

export { monaco, THEME };