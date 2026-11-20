# docs — what is here, and what to read first

This tree is `forge`, a Rust harness: twenty-four crates over one append-only
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
| [dsh](dsh-comparison.md) | how this compares with original DSH — speed claims, rc.1 install | delivered |
| [dsh vs forge](dsh-vs-forge.md) | how the two codebases actually differ — source-level, rc.5, all layers | delivered |
| [agent comparison](agent-comparison.md) | how the two agent loops differ — two independently evolved designs (DSH→forge-go→forge-rs), rc.5 | delivered |
| [agent gaps](agent-gaps.md) | what DSH has that forge-rs lacks, and how to close it — the next-wave backlog | delivered |
| [agent parity](10-agent-parity.md) | the execution roadmap for closing those gaps: waves, language, acceptance | planned, not built |
| [agent hooks](11-adr-agent-hooks.md) | ADR-0004: the two rewrite seams and the `hook/rewrite` audit row | delivered (W5) |
| [subagent port](12-subagent-port.md) | what the deep subagent port delivered (S1–S5), including the out-of-process worker backend | delivered (S1–S5) |
| [release](release.md) | what an artifact does and does not claim — release builder | delivered |
| [web ui](web-ui.md) | does a 10k-event transcript hold 60 fps — transcript owner | delivered |
| [office](office-run.md) | does the Office skill actually produce documents — skill editor | delivered |
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

`delivered` is the document's own evidence, not this table's opinion. The header
lines of [08](08-rust-port.md) and [bench](bench.md) used to contradict their own
bodies ("accepted, in progress" above a wave table that ends with every wave in;
"no baseline has been recorded" above a baseline file with three rows) and were
corrected rather than left as a known lie. [09](09-platform-tier.md) is
`partially delivered` on purpose: waves P1–P4 are in, and the one item it
deliberately leaves unbuilt is the pure-Rust document writer, which its §5 says
only becomes worth writing if a single-binary install is ever required.
[ci](ci.md) and [release](release.md) are delivered as documents, and both
now describe runs that actually happened, on hosted runners, with the run ids.
[office](office-run.md) is the same kind of record for the Office skill: what the
shipped instructions produced, what was missing, and what was never verified.
[dsh vs forge](dsh-vs-forge.md) states its version boundary up front — it reads
rc.5 source but reuses rc.1 performance numbers, and says so before drawing any
conclusion. [agent comparison](agent-comparison.md) narrows the same rc.5 source
down to the agent execution layer: loop shape, inbox, extension points, stream
accounting, cancellation, subagent, team/goal and compaction — the earlier
[dsh vs forge](dsh-vs-forge.md) covers all layers. It reads `forge-rs` as a Rust
port of `forge-go` (which was written by the DSH team), so where the two differ
it is a difference between two independently evolved designs, not a porting bug;
[agent gaps](agent-gaps.md) is the ledger of what DSH has and this tree does not,
rated P0–P2, and is the one to open when planning the next wave. Where this table
and a document disagree, the document is the source of truth and the table is the
bug. [agent parity](10-agent-parity.md) is the execution roadmap that follows the
gap ledger: it fixes the language choice (single Rust kernel, plugin bodies in any
language, TypeScript front-end), splits the work into waves W1–W6, and states each
wave's contract changes, tests and acceptance — **it is planned, not built**.

## Keeping this index from rotting

`forge help` ends by naming this file in its usage text
([crates/cli/src/lib.rs](../crates/cli/src/lib.rs) `USAGE`), and
`cargo run -q -p archcheck --offline` now fails `docs.missing` when a
`docs/<file>.md` named there is not a file on disk. A new document gets a row
here; deleting one means changing that usage line first, in the same change.
