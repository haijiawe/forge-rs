---
dir: crates/experts
layer: L1
role: expert and expert-team definitions parsed from Markdown plus YAML frontmatter
public:
  - Set
  - Expert
  - Team
  - TeamMember
  - scan
  - prompt_for
  - team_prompt
  - Error
  - ErrorKind
  - Result
  - ERROR_CODES
may_import: []
must_not:
  - depend on any forge crate or any external crate
  - resolve YAML that is not the documented scalar / flow-list / block-list subset
  - normalise, trim or re-encode a definition body
  - decide which expert a turn should summon
gate: cargo test -p forge-experts --offline
---

# forge-experts

## 做什么

An expert is a persona that can be summoned as a subagent: a Markdown file whose
frontmatter carries `name`, `slug`, `description` and optionally `division`,
`enabled` and `tools`, and whose body is the system prompt. A team is a Markdown
file that names experts and their duties plus one `coordinator`, and whose body
is the team-level instruction.

`scan(root)` walks a directory tree, parses every `.md` file, and returns a `Set`
that has been checked against itself: slugs are legal and unique across the whole
tree, every required field is present, and every team reference resolves to a
real, enabled expert. A missing root is an **empty set**, not an error, because a
machine that has never created an experts directory is a normal machine and must
still start.

`prompt_for(&set, slug)` and `team_prompt(&set, slug)` turn one entry of that set
into the exact string a caller sends to a model. Both are pure functions of the
set and the slug, so two calls give byte-identical strings; a team's members
appear in the order its file lists them, never re-sorted.

The three rules that shaped every refusal:

- **Refuse, do not warn.** Losing a `tools:` line turns a read-only reviewer into
  a persona that may run commands. Losing a `members:` line turns a four-person
  team into a three-person team without a word. Each refusal names the file and
  what to change.
- **The body is bytes.** `Expert::prompt` and `Team::prompt` are the file body
  exactly as written, including `\r\n`, tabs, a line that looks like a closing
  fence, and trailing newlines. A loader that normalised anything would make the
  prompt differ from the file a reviewer approved.
- **One namespace.** A team slug and an expert slug share one namespace, and two
  files defining the same slug are refused with both paths in the message.
  "Which one wins" is exactly the ambiguity a roster must not have.

## 公开面

- `scan(&Path) -> Result<Set>`; `Set::empty()`.
- `Set`: `experts()`, `teams()`, `expert(slug)`, `team(slug)`, `summonable(slug)`,
  `is_empty()`, `len()`. Only `scan` can build a non-empty one, so "a `Set`
  exists" means "the tree was checked".
- `Expert { slug, name, description, division, enabled, tools, prompt, path }`.
  `tools: Option<Vec<String>>`: `None` means no restriction was stated,
  `Some(vec![])` means `tools: []`. The two are different instructions and the
  assembled prompt says so.
- `Team { slug, name, description, coordinator, members, prompt, path }`,
  `TeamMember { expert, duty, description }` (`description` is the member
  expert's own, resolved at load).
- `prompt_for(&Set, slug) -> Option<String>` — the body, plus the tool
  restriction line when `tools` is present. `None` for an unknown or disabled
  slug.
- `team_prompt(&Set, slug) -> Option<String>` — the body, plus the coordinator
  instruction, plus one numbered line per member with its slug, duty and
  description. `None` for an unknown slug.
- `Error` with `kind()`, `code()` and `message()`; `ErrorKind` with `as_str()`;
  `ERROR_CODES` for the whole set of stable codes.

Codes, stable for the CLI: `missing_frontmatter`, `bad_frontmatter`,
`missing_field`, `bad_slug`, `duplicate_slug`, `unknown_member`,
`disabled_member`, `too_few_members`, `unreadable`.

## 依赖与边界

Standard library only. No forge-internal dependency, no external crate, no
`anyhow`, no async runtime. The roster is text: a dependency would only add a
version to argue about, and the brief's law 3 refuses one.

Why `forge-skill`'s reader is not reused, even though it is a same-layer edge and
would have been legal on three counts:

- its frontmatter parser and its walker are `pub(crate)`, so reuse means widening
  another crate's privacy to reach a `BTreeMap`;
- it scans exactly `root/*/SKILL.md` and never recurses, while a roster is a
  genuinely nested tree;
- its policies are the opposite of this crate's: it drops a key it cannot read
  and normalises `\r\n` to `\n` while trimming the body's leading newlines. Both
  are correct for a skill and both are disqualifying for a prompt that must be
  byte-verbatim and for a `tools:` line whose loss changes what the model may do.

The boundary is "read and check, never decide". This crate reports which experts
and teams exist and produces their prompts; which expert a turn should summon is
policy and belongs above L1.

## 不要做

- Do not support the rest of YAML. The accepted subset is: a `---` fence, scalar
  `key: value` at column 1, `key: [a, b]` flow lists, and `- item` block lists of
  scalars or of single-level `- key: value` maps. Block scalars (`|`, `>`), nested
  maps, nested lists, anchors, tags, flow maps and multi-document files are
  refused with a message, never guessed at — the body is where long text goes.
- Do not normalise a body. No newline translation, no trimming, no BOM inside the
  body. A test asserts the body survives `\r\n`, a tab, a `---` line and non-ASCII.
- Do not inline a whole file. Reads are capped at 1 MiB and an oversized file is
  refused, never truncated: a truncated prompt is a prompt nobody reviewed.
- Do not sort, dedupe or reorder members, and do not sort the prompt lines. File
  order is what was reviewed.
- Do not treat an unknown field as a warning. `tool:` mistyped for `tools:` would
  silently drop a tool restriction, so an unrecognised field is a refusal.
- Do not recurse without bounds. Depth is capped at 32 levels and a directory
  reached twice is walked once, so a wrong root or a symlink cycle cannot hang
  the loader.
- Not implemented, stated plainly: no writer (`scan` never creates or modifies a
  file; there is no `forge expert new` here), no watching or caching (each `scan`
  re-reads the tree), no inheritance between experts, no validation that a
  `tools:` name is a tool `forge-tool` actually registers — this crate does not
  import L2 and cannot know, so a typo there is caught by the registry at call
  time, not here.
