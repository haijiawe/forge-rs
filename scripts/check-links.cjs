// 检查仓库里每一份被 git 跟踪的 markdown 的相对链接。
//
// 为什么要有这个：README 是唯一「读的人会照着它跳」的文档。一条指向不存在文件的
// 链接，和一句假话是同一件事——它让读者怀疑所有别的链接。而被删除、被改名、
// 被 `git mv` 只改了引用没改文件的那些情况，没有任何编译器会把它们变红。
//
// 判据是 `git ls-files --cached --others --exclude-standard`：被跟踪的 markdown
// 加上还没提交但没被忽略的 markdown。只算 `--cached` 会让今天新写的一份 README
// 在本地一路绿灯，直到它被提交之后 CI 才红——那时的红已经不指向写它的那次编辑了。
// 归档目录、node_modules、target 里的东西仍不在此列，因为它们不是读者会打开的东西。
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

let checked = 0;
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
  });
}

console.log(`checked ${checked} relative links in ${files.length} files`);
if (problems.length === 0) {
  console.log('all links resolve');
} else {
  console.log(`${problems.length} broken:`);
  for (const problem of problems) console.log('  ' + problem);
  process.exitCode = 1;
}