---
dir: crates/skill
layer: L1
role: skill discovery and on-demand loading of SKILL.md instruction files
public:
  - Skill
  - Set
  - scan
  - Scan
  - SkillError
  - SkillErrorKind
may_import: []
must_not:
  - depend on any hearth crate or any external crate
  - recurse into skill directories or write any file
  - guess at YAML structure that is not a scalar key: value pair
gate: cargo test -p hearth-skill --offline
---

## 做什么

Discover skills at `root/*/SKILL.md`, one level below each supplied root, and
load their bodies only when asked. A missing root is a warning, not a failure:
the scan continues with the remaining roots so one bad path cannot hide the rest.

Everything that does not produce a skill is data: `Set::skipped` and
`Set::warnings` carry sorted, human-readable reasons. `Scan` never returns an
error for a filesystem problem; `Skill::load` does, because a caller asking for
a specific body needs to know that it failed.

## 公开面

`Skill { name, description, dir, path, body, files }` is `Clone + Debug +
PartialEq`, so a rendered `Set` can be compared with the Go golden byte for
byte. `Set { skills, skipped, warnings }` has the same derives.

`scan(&[PathBuf]) -> Result<Set>` is the idiomatic entry point; `Scan` is the
same function under the contract's Go-compatible spelling. `Skill::load(&self)
-> Result<Skill>` returns a copy with `body` filled and never mutates the
receiver. Errors use `SkillError` / `SkillErrorKind`.

## 依赖与边界

Standard library only. No hearth-internal dependency, no external crate, no
async runtime. All filesystem access goes through `std::fs` and `std::path`;
only the *output* strings are normalised to forward slashes so the golden is
stable on Windows and Unix.

The boundary is "discover and read, never decide": this crate reports what it
found, not which skill a turn should use. Policy and invocation belong above
L1.

## 不要做

- Do not recurse into a skill directory. `Files` lists regular sibling files
  only, excluding symlinked directories, sorted by relative path.
- Do not silently drop a structured frontmatter value. A nested or block scalar
  produces a warning and the key is removed; nothing is guessed.
- Do not read a whole unbounded file. Reads are capped at 1 MiB, with an
  incomplete UTF-8 rune at the cap trimmed, and only `Skill::load` reads a body.
- Do not let diagnostic order depend on directory iteration order. Skills,
  skipped, and warnings are all sorted before `Set` is returned.
