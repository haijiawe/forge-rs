# docs — what is here, and what to read first

This tree is `forge`, a Rust harness: twenty-three crates over one append-only
event log, with [architecture.yaml](../architecture.yaml) stating which crate may
import which and [tools/archcheck](../tools/archcheck) failing the build when an
edge points up. What is frozen is [docs/contracts/](contracts/README.md) — the v1
event format, the CLI text, the HTTP/WS protocol, and the M4–M6 crate surfaces —
because a promise another implementation is held to cannot move quietly. The
corpora under [tests/](../tests) are what proves the Rust readers keep that
promise: 22 event cases (10 accept, 12 refuse), 34 HTTP cases, 8 WS cases,
5 fixture logs, 2 LLM stream fixtures and 2 goldens, language-neutral and copied
in before the Go tree was deleted on 2026-11-17.

## The documents

| Doc | What question it answers, and who reads it | State |
| --- | --- | --- |
| [08 port](08-rust-port.md) | why Rust, which crate owns what — crate authors | delivered |
| [09 tier](09-platform-tier.md) | what is still missing — next-wave planner | partially delivered |
| [bench](bench.md) | is the build slower than the baseline — perf owner | delivered |
| [ci](ci.md) | what CI runs, and what was only rehearsed — workflow editor | delivered |
| [dsh](dsh-comparison.md) | how this compares with original DSH — speed claims | delivered |
| [release](release.md) | what an artifact does and does not claim — release builder | delivered |
| [web ui](web-ui.md) | does a 10k-event transcript hold 60 fps — transcript owner | delivered |
| [contracts](contracts/README.md) | what is frozen — wire/CLI/API editors | delivered |

The [contracts/](contracts/README.md) directory holds seven files:
[events-v1.md](contracts/events-v1.md) (envelope, payloads, replay),
[cli.md](contracts/cli.md) (commands, help text, exit codes),
[api.md](contracts/api.md) (HTTP/WS paths, statuses, frames),
[m4-surface.md](contracts/m4-surface.md) (index, import, fork, compaction),
[m5-surface.md](contracts/m5-surface.md) (MCP, skills, subagent),
[m6-surface.md](contracts/m6-surface.md) (sandbox tiers, danger list, doctor),
plus the directory's own [README.md](contracts/README.md).

## States, read honestly

`delivered` is the document's own evidence, not this table's opinion. Three
header lines in these files are already stale, each contradicted inside the same
file: [08](08-rust-port.md) opens "accepted, in progress" while its wave table
ends with all six waves delivered; [09](09-platform-tier.md) opens "nothing here
is implemented" while §5 records P1–P3 shipped and P4 as a plan — which is why
its state is `partially delivered`; [bench](bench.md) says no baseline was ever
recorded while [scripts/bench/baseline.json](../scripts/bench/baseline.json)
holds one from a settled tree. [ci](ci.md) and [release](release.md) are
delivered as documents, but both say plainly that no hosted runner has executed
them. Where this table and a document disagree, the document is the source of
truth and the table is the bug.

## Keeping this index from rotting

`forge help` ends by naming this file in its usage text
([crates/cli/src/lib.rs](../crates/cli/src/lib.rs) `USAGE`), and
`cargo run -q -p archcheck --offline` now fails `docs.missing` when a
`docs/<file>.md` named there is not a file on disk. A new document gets a row
here; deleting one means changing that usage line first, in the same change.
