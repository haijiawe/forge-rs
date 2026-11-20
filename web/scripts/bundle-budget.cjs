// Two bundle budgets, because the workbench has two kinds of JavaScript.
//
// The promise in the UI design is about what a reader waits for: the shell,
// the transcript and the composer. That payload is `index.html` plus whatever
// it statically imports, and it is held to the documented 600 KiB gzip.
//
// Monaco is different: it is a dynamic `import()`, so it is not fetched until
// somebody opens a file, and its 90-odd chunks are grammar definitions loaded
// per language. Counting it against the eager budget would either delete the
// budget or forbid the editor; both are worse than naming the second number.
// So the lazy set gets its own ceiling, printed on every run, and the eager
// number stays the one the design document promises.
//
// Usage: node scripts/bundle-budget.cjs   (from web/, after `npm run build`)
//   --eager-gzip <bytes>  --eager-raw <bytes>  --total-gzip <bytes>

const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

const DIST = path.join(__dirname, '..', 'dist');
const ASSETS = path.join(DIST, 'assets');

const KIB = 1024;
const LIMITS = {
  eagerGzip: 600 * KIB,
  eagerRaw: 700 * 1000,
  totalGzip: 1600 * KIB,
};

function fail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

function gzipLength(bytes) {
  return zlib.gzipSync(bytes, { level: 9 }).length;
}

function entryScripts(html) {
  const names = [];
  const pattern = /<script[^>]*\ssrc="\/(assets\/[^"]+\.js)"/g;
  let match;
  while ((match = pattern.exec(html)) !== null) names.push(path.basename(match[1]));
  return names;
}

// Vite inlines static imports into the chunk that needs them, so this usually
// finds nothing. It runs anyway: the day a shared chunk appears, the eager
// number must count it rather than quietly under-report.
function staticImports(text) {
  const names = new Set();
  const pattern = /["']\.\/([A-Za-z0-9._-]+\.js)["']/g;
  let match;
  while ((match = pattern.exec(text)) !== null) names.add(match[1]);
  return names;
}

function collectEager(entries) {
  const seen = new Set();
  const queue = [...entries];
  while (queue.length > 0) {
    const name = queue.pop();
    if (seen.has(name)) continue;
    const file = path.join(ASSETS, name);
    if (!fs.existsSync(file)) continue;
    seen.add(name);
    for (const next of staticImports(fs.readFileSync(file, 'utf8'))) queue.push(next);
  }
  return seen;
}

function measure(names) {
  let raw = 0;
  let gzip = 0;
  for (const name of names) {
    const bytes = fs.readFileSync(path.join(ASSETS, name));
    raw += bytes.length;
    gzip += gzipLength(bytes);
  }
  return { raw, gzip, count: names.size };
}

function main() {
  if (!fs.existsSync(path.join(DIST, 'index.html'))) {
    fail(`no built UI at ${DIST}: run "npm run build" first`);
  }
  if (!fs.existsSync(ASSETS)) fail(`no assets directory at ${ASSETS}`);

  const html = fs.readFileSync(path.join(DIST, 'index.html'), 'utf8');
  const entries = entryScripts(html);
  if (entries.length === 0) fail('index.html names no script: the build is not what this check measures');

  const eager = measure(collectEager(entries));
  const all = fs
    .readdirSync(ASSETS)
    .filter((name) => name.endsWith('.js'));
  const total = measure(new Set(all));
  const lazy = { raw: total.raw - eager.raw, gzip: total.gzip - eager.gzip, count: total.count - eager.count };

  const rows = [
    ['eager', eager, LIMITS.eagerGzip, LIMITS.eagerRaw],
    ['total', total, LIMITS.totalGzip, null],
  ];
  for (const [label, value, gzipCap, rawCap] of rows) {
    console.log(
      `${label.padEnd(6)} ${String(value.count).padStart(3)} file(s)  ` +
        `${String(value.raw).padStart(9)} raw  ${String(value.gzip).padStart(8)} gzip` +
        (gzipCap === null ? '' : `  (cap ${gzipCap} gzip${rawCap === null ? '' : `, ${rawCap} raw`})`),
    );
  }
  console.log(
    `lazy   ${String(lazy.count).padStart(3)} file(s)  ` +
      `${String(lazy.raw).padStart(9)} raw  ${String(lazy.gzip).padStart(8)} gzip  ` +
      '(dynamic imports: Monaco and its grammars, fetched when a file is opened)',
  );

  const problems = [];
  if (eager.gzip > LIMITS.eagerGzip) {
    problems.push(`eager JavaScript is ${eager.gzip} bytes gzip, over the ${LIMITS.eagerGzip} byte budget`);
  }
  if (eager.raw > LIMITS.eagerRaw) {
    problems.push(`eager JavaScript is ${eager.raw} bytes raw, over the ${LIMITS.eagerRaw} byte budget`);
  }
  if (total.gzip > LIMITS.totalGzip) {
    problems.push(
      `total JavaScript is ${total.gzip} bytes gzip, over the ${LIMITS.totalGzip} byte lazy budget ` +
        '(raise it in this file, or stop bundling what pushed it over)',
    );
  }
  if (problems.length > 0) fail(problems.join('; '));
}

main();