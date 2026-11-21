// 检查仓库里每一份被 git 跟踪的 markdown：相对链接，以及反引号里写的仓库路径。
//
// 为什么要有这个：README 是唯一「读的人会照着它跳」的文档。一条指向不存在文件的
// 链接，和一句假话是同一件事——它让读者怀疑所有别的链接。而被删除、被改名、
// 被 `git mv` 只改了引用没改文件的那些情况，没有任何编译器会把它们变红。
//
// 只查链接语法是不够的。实测过一次：仓库里 239 条相对链接全部解析成功，而同时
// 有四处散文里的死引用（`docs/audit-m7.md`、`tests/appcontainer.rs`、
// `tests/reference-node/smoke.py`、`tests/identity.rs`）——它们都写在反引号里，
// 不是链接。读者不会因为「不是链接」就少被骗一次，所以这里也查它们。
//
// 判据是 `git ls-files --cached --others --exclude-standard`：被跟踪的 markdown
// 加上还没提交但没被忽略的 markdown。只算 `--cached` 会让今天新写的一份 README
// 在本地一路绿灯，直到它被提交之后 CI 才红——那时的红已经不指向写它的那次编辑了。
//
// 用法：node scripts/check-links.cjs [仓库根]
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const root = path.resolve(process.argv[2] || path.join(__dirname, '..'));

function trackedMarkdown() {
  const output = execFileSync(
    'git',
    ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '*.md'],
    {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 32 * 1024 * 1024,
    },
  );
  const seen = new Set();
  return output
    .split('\0')
    .filter((entry) => entry.length > 0)
    .filter((entry) => (seen.has(entry) ? false : seen.add(entry)))
    .map((entry) => path.join(root, entry));
}

const linkPattern = /\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;
// ``` 与 ~~~ 都算围栏；围栏里的内容是示例，不是链接。
const fencePattern = /^\s*(```|~~~)/;
// 反引号里的行内代码。只取单反号的：`` 这种嵌套形式留给人读。
const codePattern = /`([^`\n]+)`/g;

// 一行散文里的路径，只有以这些顶层目录开头才算「仓库路径」。这条限制是刻意的：
// `hearth-core`、`serde_json`、`/api/sessions` 都不是文件，把它们当路径查会制造
// 一堆噪声，而噪声会训练人忽略这个检查。
const TOP_LEVEL = [
  '.github',
  'apps',
  'bin',
  'crates',
  'docs',
  'examples',
  'scripts',
  'skills',
  'tests',
  'tools',
  'web',
];

// 仓库根上的文件名，写成反引号时也值得查。
const ROOT_FILES = [
  'AGENTS.md',
  'Cargo.lock',
  'Cargo.toml',
  'Makefile',
  'README.md',
  'architecture.yaml',
  'check.ps1',
];

// 两类「不在本树里，但提到它有正当理由」的路径。它们不是豁免，是**有条件的通过**：
// 同一行必须出现限定词，否则照样报出来——一条不带限定的 `docs/deps.md`，读者会去
// 找本树的 docs/ 目录，然后什么也找不到，这跟写死引用是同一种伤害。
//
// 每一类都逐条列出（不用通配），这样新增一条必须有人签字。
const RETIRED = [
  // 已退役的 Go 树（2026-11-17 删除）。它的文档与测试文件没有随树迁过来。
  { token: 'forge-go/cmd/forge/', qualifier: null },
  { token: 'forge-go/internal/server', qualifier: null },
  { token: 'internal/protocol/protocol.go', qualifier: null },
  { token: 'docs/deps.md', qualifier: /已退役|退役的 Go 树|Go 树|retired/ },
  { token: 'docs/05-roadmap.md', qualifier: /已退役|退役的 Go 树|Go 树|retired/ },
  {
    token: 'tests/conformance/conformance_test.go',
    qualifier: /已退役|退役的 Go 树|Go 树|retired/,
  },
  // DSH 参考实现仓库（D:\reference\deepseek-harness-master），不是本仓库。
  { token: 'docs/architecture.md', qualifier: /DSH|deepseek-harness/ },
  { token: 'docs/subsystems/webhook.md', qualifier: /DSH|deepseek-harness/ },
  // 本仓库里被改过名的文件（2026-11-21 forge 改成 hearth）。旧名出现在删除台账里
  // 是它的职责，但同一行必须说清楚这是个旧名，否则读者会去点它。
  { token: 'docs/dsh-vs-forge.md', qualifier: /旧名|改名为|已改名|renamed/ },
];

// 能当文件读的后缀。一条反引号路径要么带这些后缀之一，要么以 `/` 结尾表示目录，
// 否则它不是路径——`tools/call` 是 JSON-RPC 方法名，`tools/` 才是目录。这条规矩
// 是拿真实仓库试出来的：不写它，MCP 的方法名会全部被当成断链。
const EXTENSIONS = [
  '.cjs',
  '.css',
  '.exe',
  '.go',
  '.html',
  '.ico',
  '.js',
  '.json',
  '.jsonl',
  '.lock',
  '.md',
  '.mjs',
  '.mod',
  '.png',
  '.proto',
  '.ps1',
  '.py',
  '.rs',
  '.sh',
  '.svg',
  '.toml',
  '.ts',
  '.tsx',
  '.txt',
  '.yaml',
  '.yml',
];

// 构建产物：在本地存在，在干净检出里不存在。把 `bin/hearth.exe` 报成断链是错的
// ——它确实在那里，只是需要一个 `cargo build`。CI 在干净检出上跑这个脚本，所以这
// 条名单是被 CI 教出来的：第一次带上反引号检查时，它报了 12 条，全是这一种。
// 只跳过产物本身，不跳过整个目录：`bin/README.md` 是被跟踪的，写错它要能报出来。
const GENERATED = [
  'bin/hearth.exe',
  'bin/hearth',
  'target/',
  'web/dist',
  'web/node_modules/',
];

// 生成物的前缀（目录形态）：`web/dist/assets/` 这种要按前缀看。
const GENERATED_PREFIXES = ['target/', 'web/dist/', 'web/node_modules/'];

function isGenerated(bare) {
  if (GENERATED.includes(bare) || GENERATED.includes(`${bare}/`)) return true;
  return GENERATED_PREFIXES.some((prefix) => bare.startsWith(prefix));
}

function looksLikePath(token) {
  const clean = token.trim();
  if (clean.length === 0 || clean.length > 200) return false;
  // 通配、占位、命令、章节号、URL：都不是一个具体文件。
  // `…` 与 `...` 也是占位：写出来就是在说「这里随情况变」，不是给出一个文件。
  if (/[*?<>{}[\]$|"'`\s]|…|\.\.\./.test(clean)) return false;
  if (/^(https?|mailto|file):/.test(clean)) return false;
  if (clean.startsWith('#') || clean.startsWith('-')) return false;
  if (/\b(cargo|node|npm|git|pwsh|make)\b/.test(clean)) return false;
  const bare = clean.replace(/:\d+(-\d+)?$/, '');
  const first = bare.split('/')[0];
  const isRootFile = !bare.includes('/') && ROOT_FILES.includes(bare);
  if (!TOP_LEVEL.includes(first) && !isRootFile) return false;
  // 后缀或目录斜杠，二者必居其一。
  const trimmed = bare.replace(/[,.;]$/, '');
  if (trimmed.endsWith('/')) return true;
  return EXTENSIONS.some((extension) => trimmed.toLowerCase().endsWith(extension));
}

// 这条路径是不是「按条件通过」的那一类。返回 'skip'（无条件的）、'qualified'（要有
// 限定词）、或 null（不在名单里，照常查）。
function conditioning(token, line) {
  const bare = token.trim().replace(/[,.;]$/, '');
  // 构建产物在干净检出里不存在是正常的，不是断链。
  if (isGenerated(bare)) return 'skip';
  for (const entry of RETIRED) {
    const matches = bare === entry.token || bare.startsWith(entry.token);
    if (!matches) continue;
    if (entry.qualifier === null) return 'skip';
    return entry.qualifier.test(line) ? 'skip' : 'qualified';
  }
  return null;
}

let checked = 0;
let pathsChecked = 0;
const problems = [];

const files = trackedMarkdown();
for (const file of files) {
  if (!fs.existsSync(file)) {
    problems.push(`${path.relative(root, file)}: tracked but not on disk`);
    continue;
  }
  const lines = fs.readFileSync(file, 'utf8').split('\n');
  let inFence = false;
  lines.forEach((line, index) => {
    if (fencePattern.test(line)) {
      inFence = !inFence;
      return;
    }
    if (inFence) return;
    for (const match of line.matchAll(linkPattern)) {
      // 尖括号形式允许路径里有空格： [名字](<a b.md>)
      let target = match[1].replace(/^<|>$/g, '');
      if (/^[a-z][a-z0-9+.-]*:/i.test(target)) continue; // http:, https:, mailto:, ...
      if (target.startsWith('#')) continue; // 本文内的锚点
      const bare = target.split('#')[0];
      if (bare === '') continue;
      checked += 1;
      const resolved = bare.startsWith('/')
        ? path.join(root, bare.slice(1))
        : path.resolve(path.dirname(file), bare);
      if (!fs.existsSync(resolved)) {
        problems.push(`${path.relative(root, file)}:${index + 1} -> ${match[1]}`);
      }
    }
    for (const match of line.matchAll(codePattern)) {
      const token = match[1];
      if (!looksLikePath(token)) continue;
      const verdict = conditioning(token, line);
      if (verdict === 'skip') continue;
      if (verdict === 'qualified') {
        // 名字指的是别处的东西，而这一行没说清是别处——这比找不到文件更糟：
        // 读者会去本树里找，找不到，然后不再相信任何引用。
        problems.push(
          `${path.relative(root, file)}:${index + 1} -> \`${token}\` (names a file outside this tree; say so on the same line)`,
        );
        continue;
      }
      const clean = token.trim().replace(/[,.;]$/, '');
      // `path:123` 与 `path:member` 都是在指认文件里的位置，文件本身是前半段。
      const variants = [clean, clean.replace(/:\d+(-\d+)?$/, ''), clean.replace(/:[\w:]+$/, '')];
      pathsChecked += 1;
      // 散文里的仓库路径可能是相对根写的，也可能相对本文。两种都试：文档里
      // `crates/x` 是相对根的惯例，而 crate README 里会写同目录的兄弟。
      const found = variants.some((variant) =>
        [path.join(root, variant), path.resolve(path.dirname(file), variant)].some((candidate) =>
          fs.existsSync(candidate),
        ),
      );
      if (!found) {
        problems.push(`${path.relative(root, file)}:${index + 1} -> \`${token}\``);
      }
    }
  });
}

console.log(
  `checked ${checked} relative links and ${pathsChecked} backticked paths in ${files.length} files`,
);
if (problems.length === 0) {
  console.log('all links resolve');
} else {
  console.log(`${problems.length} broken:`);
  for (const problem of problems) console.log('  ' + problem);
  process.exitCode = 1;
}