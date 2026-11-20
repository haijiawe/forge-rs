// Commit, push, and align — one command, because this network cannot `git push`.
//
// `git push` and `git fetch` both die against github.com:443 here (connection
// reset), so the only working channel is the REST API. That has one annoying
// consequence: a commit created through the API is a *new object*, so its SHA
// differs from the local one even though the tree is byte-identical. Left
// alone, `git status` then reports the branch as diverged forever.
//
// This script closes that loop:
//
//   1. commit the working tree (unless --no-commit)
//   2. build the remote tree by diffing blob SHAs, assert it equals the local
//      tree, commit it on top of the remote head, move the ref
//   3. rebuild that remote commit object locally (the API normalises timestamps
//      to `Z`; we try the plausible offsets) and move the local branch onto it
//
// Step 3 is the part a plain push would not need, and it is why local and remote
// SHAs end up equal: after this runs, `git status` says `master...origin/master`
// with nothing ahead and nothing behind.
//
// usage:
//   node scripts/sync.cjs "commit message"     commit, push, align
//   node scripts/sync.cjs --message-file f.txt commit with a multi-paragraph message
//   node scripts/sync.cjs --no-commit          push and align the current HEAD
//   node scripts/sync.cjs --dry "message"      say what would happen, write nothing
//
// The token comes from $GITHUB_PAT, or from `GITHUB_PAT:` in
// $HEARTH_CREDENTIALS (default $HEARTH_CREDENTIALS or D:\dsh\.credentials.yaml).
// Nothing is logged.

const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const REPO = path.resolve(__dirname, '..');
const SLUG = process.env.HEARTH_REPO || 'haijiawe/hearth';
const GIT = process.env.HEARTH_GIT || 'git';
const API = `https://api.github.com/repos/${SLUG}`;
const OFFSETS = ['+0000', '+0800', '-0700'];

const args = process.argv.slice(2);
const dry = args.includes('--dry');
const noCommit = args.includes('--no-commit');
// A message file exists because a multi-paragraph message is unreadable as one
// shell argument: backticks, quotes and newlines all survive a file and none of
// them survive every shell on the way here.
const messageFileIndex = args.indexOf('--message-file');
const messageFile = messageFileIndex >= 0 ? args[messageFileIndex + 1] : null;
const words = args.filter((a, index) => {
  if (a.startsWith('--')) return false;
  if (messageFileIndex >= 0 && index === messageFileIndex + 1) return false;
  return true;
});
const message = messageFile ? fs.readFileSync(path.resolve(messageFile), 'utf8').trim() : words.join(' ').trim();

function git(argv, opts = {}) {
  return execFileSync(GIT, argv, { cwd: REPO, maxBuffer: 256 << 20, ...opts });
}
function gitText(argv) {
  return git(argv, { encoding: 'utf8' }).trim();
}

function token() {
  if (process.env.GITHUB_PAT) return process.env.GITHUB_PAT;
  const file = process.env.HEARTH_CREDENTIALS || path.join('D:', 'dsh', '.credentials.yaml');
  const m = /GITHUB_PAT:\s*(\S+)/.exec(fs.readFileSync(file, 'utf8'));
  if (!m) throw new Error(`no GITHUB_PAT in ${file}`);
  return m[1];
}

let bearer = null;
async function call(method, url, body) {
  bearer ??= token();
  const res = await fetch(API + url, {
    method,
    headers: {
      Authorization: `Bearer ${bearer}`,
      Accept: 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28',
      'Content-Type': 'application/json',
      'User-Agent': 'hearth-sync',
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${url} -> ${res.status}: ${text.slice(0, 600)}`);
  return text ? JSON.parse(text) : {};
}

function dirty() {
  return gitText(['status', '--porcelain']) !== '';
}

function localTree() {
  const out = git(['ls-tree', '-r', '-z', 'HEAD']);
  const map = new Map();
  for (const rec of out.toString('utf8').split('\0')) {
    if (!rec) continue;
    const tab = rec.indexOf('\t');
    const [mode, , sha] = rec.slice(0, tab).split(' ');
    map.set(rec.slice(tab + 1), { sha, mode });
  }
  return map;
}

function commitFields(sha) {
  const raw = gitText(['cat-file', '-p', sha]);
  const [head, ...body] = raw.split('\n\n');
  const lines = head.split('\n');
  return {
    sha,
    tree: lines[0].replace(/^tree\s+/, ''),
    parents: lines.filter((l) => l.startsWith('parent ')).map((l) => l.slice(7)),
    text: body.join('\n\n'),
  };
}

// --- step 2: push -----------------------------------------------------------

async function push(remoteHead) {
  const localSha = gitText(['rev-parse', 'HEAD']);
  const local = localTree();
  const localTreeSha = gitText(['rev-parse', 'HEAD^{tree}']);

  const parent = await call('GET', `/git/commits/${remoteHead}`);
  const remote = new Map();
  const listing = await call('GET', `/git/trees/${parent.tree.sha}?recursive=1`);
  if (listing.truncated) throw new Error('remote tree listing was truncated; refusing to guess');
  for (const e of listing.tree) if (e.type === 'blob') remote.set(e.path, { sha: e.sha, mode: e.mode });

  const added = [];
  const updated = [];
  const deleted = [];
  for (const [p, l] of local) {
    const r = remote.get(p);
    if (!r) added.push(p);
    else if (r.sha !== l.sha || r.mode !== l.mode) updated.push(p);
  }
  for (const p of remote.keys()) if (!local.has(p)) deleted.push(p);

  console.log(`local tree ${localTreeSha.slice(0, 7)}; remote head ${remoteHead.slice(0, 7)} (tree ${parent.tree.sha.slice(0, 7)})`);
  console.log(`  ${added.length} added, ${updated.length} updated, ${deleted.length} deleted, ${local.size} files total`);
  for (const p of added) console.log(`    A ${p}`);
  for (const p of updated) console.log(`    M ${p}`);
  for (const p of deleted) console.log(`    D ${p}`);

  if (parent.tree.sha === localTreeSha && added.length + updated.length + deleted.length === 0) {
    console.log('  remote already has this exact tree; nothing to push');
    return remoteHead;
  }
  if (dry) return null;

  const entries = [];
  for (const p of [...added, ...updated]) {
    const blob = git(['cat-file', 'blob', `${localSha}:${p}`]);
    const { sha } = await call('POST', '/git/blobs', { content: blob.toString('base64'), encoding: 'base64' });
    entries.push({ path: p, mode: local.get(p).mode, type: 'blob', sha });
  }
  for (const p of deleted) entries.push({ path: p, mode: '100644', type: 'blob', sha: null });

  const tree = await call('POST', '/git/trees', { base_tree: parent.tree.sha, tree: entries });
  if (tree.sha !== localTreeSha) {
    throw new Error(`tree mismatch: github built ${tree.sha}, local HEAD is ${localTreeSha}; nothing was moved`);
  }
  console.log(`  tree ${tree.sha.slice(0, 7)} matches local`);

  const commit = await call('POST', '/git/commits', {
    message: commitFields(localSha).text,
    tree: tree.sha,
    parents: [remoteHead],
    author: {
      name: gitText(['show', '-s', '--format=%an', localSha]),
      email: gitText(['show', '-s', '--format=%ae', localSha]),
      date: gitText(['show', '-s', '--format=%aI', localSha]),
    },
  });
  await call('PATCH', '/git/refs/heads/master', { sha: commit.sha, force: false });
  console.log(`  pushed ${commit.sha.slice(0, 7)}`);
  return commit.sha;
}

// --- step 3: align ----------------------------------------------------------

function have(sha) {
  try {
    git(['cat-file', '-e', `${sha}^{commit}`], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

function stamp(iso, tz) {
  return `${Math.floor(Date.parse(iso) / 1000)} ${tz}`;
}

function rebuild(remote, trailing) {
  const header = [`tree ${remote.tree.sha}`];
  for (const p of remote.parents || []) header.push(`parent ${p.sha}`);
  header.push(`author ${remote.author.name} <${remote.author.email}> ${stamp(remote.author.date, tzOf())}`);
  header.push(`committer ${remote.committer.name} <${remote.committer.email}> ${stamp(remote.committer.date, tzOf())}`);
  const body = remote.message.endsWith('\n') ? remote.message : `${remote.message}\n`;
  // GitHub hands the message back without its trailing newline and normalises
  // the date to `Z`; both have to be tried, not assumed.
  const text = `${header.join('\n')}\n\n${trailing ? body : body.slice(0, -1)}`;
  const sha = execFileSync(GIT, ['hash-object', '-w', '-t', 'commit', '--stdin'], {
    cwd: REPO,
    input: text,
    encoding: 'utf8',
  }).trim();
  return { sha, text };
}

// The API hands back `...Z` for everything, so the offset has to be guessed per
// commit rather than passed in. Callers loop over OFFSETS; this returns the one
// currently being tried.
let attemptTz = OFFSETS[0];
function tzOf() {
  return attemptTz;
}

async function align(remoteHead) {
  const missing = [];
  let sha = remoteHead;
  for (;;) {
    const commit = await call('GET', `/git/commits/${sha}`);
    if (have(sha)) break;
    missing.push(commit);
    if (!commit.parents || commit.parents.length === 0) break;
    sha = commit.parents[0].sha;
  }

  for (const commit of missing.reverse()) {
    let built = null;
    for (const trailing of [true, false]) {
      for (const tz of OFFSETS) {
        attemptTz = tz;
        const candidate = rebuild(commit, trailing);
        if (candidate.sha === commit.sha) {
          built = candidate;
          break;
        }
      }
      if (built) break;
    }
    if (!built) {
      throw new Error(
        `cannot reproduce ${commit.sha.slice(0, 7)} locally; refusing to move the branch onto an object I could not verify`,
      );
    }
    console.log(`  rebuilt ${commit.sha.slice(0, 7)} (${attemptTz})`);
  }

  for (const parent of [remoteHead]) {
    if (!have(parent)) throw new Error(`rebuilt chain does not reach ${parent.slice(0, 7)}`);
  }

  const localTreeSha = gitText(['rev-parse', 'HEAD^{tree}']);
  const remoteTreeSha = (await call('GET', `/git/commits/${remoteHead}`)).tree.sha;
  if (localTreeSha !== remoteTreeSha) {
    throw new Error(`refusing to move the branch: local tree ${localTreeSha} != remote tree ${remoteTreeSha}`);
  }

  if (dry) return remoteHead;
  git(['update-ref', 'refs/heads/master', remoteHead]);
  try {
    git(['update-ref', 'refs/remotes/origin/master', remoteHead]);
  } catch {
    // A missing remote-tracking ref is cosmetic; the branch ref is the one that matters.
  }
  git(['reset', '--mixed', remoteHead]);
  return remoteHead;
}

// --- main -------------------------------------------------------------------

(async () => {
  if (!dry && !noCommit && dirty()) {
    if (!message) {
      console.error('the working tree has changes; pass a commit message, or --no-commit');
      process.exit(2);
    }
    git(['add', '-A']);
    git(['-c', 'user.name=forge', '-c', 'user.email=forge@local', 'commit', '-m', message, '--quiet']);
    console.log(`committed ${gitText(['rev-parse', '--short', 'HEAD'])}: ${message.split('\n')[0]}`);
  } else if (dirty()) {
    console.log(`  (working tree has uncommitted changes; they are not part of this push)`);
  }

  const ref = await call('GET', '/git/ref/heads/master');
  const remoteHead = ref.object.sha;
  console.log(`remote head ${remoteHead.slice(0, 7)}`);

  const pushed = await push(remoteHead);
  if (pushed === null) {
    console.log('dry run, nothing written');
    return;
  }

  console.log(`aligning local branch onto ${pushed.slice(0, 7)}`);
  const head = await align(pushed);

  const tree = gitText(['rev-parse', 'HEAD^{tree}']);
  const status = gitText(['status', '--porcelain']);
  const branch = gitText(['status', '-sb']).split('\n')[0];
  console.log('');
  console.log(`HEAD   ${head}`);
  console.log(`tree   ${tree}`);
  console.log(`branch ${branch}`);
  console.log(`dirty  ${status === '' ? 'none' : `${status.split('\n').length} file(s)`}`);
  process.exit(status === '' && head === pushed ? 0 : 1);
})().catch((err) => {
  console.error(err.stack || String(err));
  process.exit(1);
});