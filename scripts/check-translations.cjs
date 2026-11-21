#!/usr/bin/env node
// 检查「原文 + 中文翻译」这一对文件没有走散。
//
// 为什么需要它：`docs/contracts/` 里的三份 wire 契约各自有一份完整中文翻译。
// 翻译一旦和原文分叉，它就从「帮助读者」变成「第二个真相」——而这类分叉不会自己
// 报错，因为两份文件都语法正确、链接也都能解析。所以这里用四条结构性比对来抓它：
//
//   1. 围栏代码块必须逐字节相同（代码不该被翻译）。
//   2. 标题层级序列必须相同（章节被加了或删了，这里会红）。
//   3. 表格行数必须相同（契约表格少一行，等于少一条规则）。
//   4. 两份文件必须互相指名（原文顶部有一次跳转，译文顶部声明自己是翻译）。
//
// 它刻意不比对散文：那做不到，也不该假装做到了。它守的是结构，散文只能靠人。
//
// 用法：node scripts/check-translations.cjs [仓库根]
// 退出码：0 全部一致；1 有分叉。

'use strict';

const fs = require('fs');
const path = require('path');

const root = process.argv[2] ? path.resolve(process.argv[2]) : path.join(__dirname, '..');
const SCOPE = path.join(root, 'docs', 'contracts');

/** 把一份 markdown 拆成「围栏代码块」与「其余行」。 */
function splitFences(text) {
  const lines = text.split('\n');
  const blocks = [];
  const prose = [];
  let open = null;
  for (const line of lines) {
    const fence = /^\s*(```|~~~)/.exec(line);
    if (open === null) {
      if (fence) {
        open = { marker: fence[1], lines: [line] };
      } else {
        prose.push(line);
      }
    } else {
      open.lines.push(line);
      if (fence && fence[1] === open.marker) {
        blocks.push(open.lines.join('\n'));
        open = null;
      }
    }
  }
  if (open !== null) {
    // 未闭合的围栏：照样算一块，让比对去发现差异，而不是静默丢掉。
    blocks.push(open.lines.join('\n'));
  }
  return { blocks, prose };
}

/** 标题层级序列，例如 [1,2,2,3,2]。 */
function headingLevels(prose) {
  const levels = [];
  for (const line of prose) {
    const m = /^(#{1,6})\s+\S/.exec(line);
    if (m) levels.push(m[1].length);
  }
  return levels;
}

/** 表格行数（以 `|` 开头的行）。 */
function tableRows(prose) {
  return prose.filter((line) => /^\s*\|/.test(line)).length;
}

function preview(block) {
  const body = block.split('\n').slice(1).filter((line) => line.trim() !== '');
  const first = body.length > 0 ? body[0] : '(空代码块)';
  return `${block.length} bytes, 首行内容 ${JSON.stringify(first.slice(0, 60))}`;
}

function compareStructure(en, zh) {
  const problems = [];

  const a = splitFences(en);
  const b = splitFences(zh);

  if (a.blocks.length !== b.blocks.length) {
    problems.push(
      `代码块数量不同：原文 ${a.blocks.length} 块，译文 ${b.blocks.length} 块`,
    );
  }
  const shared = Math.min(a.blocks.length, b.blocks.length);
  for (let i = 0; i < shared; i += 1) {
    if (a.blocks[i] !== b.blocks[i]) {
      problems.push(
        `第 ${i + 1} 个代码块不一致：原文 ${preview(a.blocks[i])} / 译文 ${preview(b.blocks[i])}`,
      );
      break;
    }
  }

  const enHeads = headingLevels(a.prose);
  const zhHeads = headingLevels(b.prose);
  if (enHeads.join(',') !== zhHeads.join(',')) {
    problems.push(
      `标题层级序列不同：原文 [${enHeads.join(',')}]，译文 [${zhHeads.join(',')}]`,
    );
  }

  const enRows = tableRows(a.prose);
  const zhRows = tableRows(b.prose);
  if (enRows !== zhRows) {
    problems.push(`表格行数不同：原文 ${enRows} 行，译文 ${zhRows} 行`);
  }

  return problems;
}

function assertMutualNaming(enName, en, zhName, zh) {
  const problems = [];
  if (!en.includes(`(${zhName})`)) {
    problems.push(`原文没有链到译文：应在顶部加一个指向 ${zhName} 的链接`);
  }
  if (!zh.includes(`(${enName})`)) {
    problems.push(`译文没有声明自己是 ${enName} 的翻译：顶部应链回原文`);
  }
  return problems;
}

function main() {
  if (!fs.existsSync(SCOPE)) {
    console.error(`check-translations: 找不到目录 ${SCOPE}`);
    process.exit(2);
  }

  const names = fs.readdirSync(SCOPE).filter((name) => name.endsWith('.zh.md')).sort();
  if (names.length === 0) {
    console.log('check-translations: 这里没有译文，没什么可比的');
    return;
  }

  let failed = 0;
  const notes = [];

  for (const zhName of names) {
    const enName = zhName.slice(0, -'.zh.md'.length) + '.md';
    const zhPath = path.join(SCOPE, zhName);
    const enPath = path.join(SCOPE, enName);

    if (!fs.existsSync(enPath)) {
      failed += 1;
      notes.push(`${zhName}: 没有对应的原文 ${enName}`);
      continue;
    }

    const en = fs.readFileSync(enPath, 'utf8');
    const zh = fs.readFileSync(zhPath, 'utf8');

    const problems = [
      ...compareStructure(en, zh),
      ...assertMutualNaming(enName, en, zhName, zh),
    ];

    if (problems.length > 0) {
      failed += 1;
      for (const problem of problems) {
        notes.push(`${enName} ↔ ${zhName}: ${problem}`);
      }
    }
  }

  console.log(
    `checked ${names.length} translation pair(s) in ${path.relative(root, SCOPE) || '.'}`,
  );
  if (notes.length === 0) {
    console.log('translations match the originals');
    return;
  }
  console.log(`${notes.length} problem(s):`);
  for (const note of notes) console.log(`  ${note}`);
  process.exit(1);
}

main();