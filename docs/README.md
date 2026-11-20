# docs — what is here, and what to read first

This tree is `hearth`, a Rust harness: twenty-four crates over one append-only
event log, with [architecture.yaml](../architecture.yaml) stating which crate may
import which and [tools/archcheck](../tools/archcheck) failing the build when an
edge points up. What is frozen is [docs/contracts/](contracts/README.md) — the v1
event format, the CLI text, the HTTP/WS protocol, and the M4–M6 crate surfaces —
because a promise another implementation is held to cannot move quietly. The
corpora under [tests/](../tests) are what proves the Rust readers keep that
promise: 32 event cases, 34 HTTP cases, 8 WS cases, 5 fixture logs, 3 goldens
under `tests/golden/` and `crates/agent/tests/golden/`, and the SSE transcripts
inline in `crates/llm/tests/openai.rs` — language-neutral, and copied in before
the Go tree was deleted on 2026-11-17.

Every document below is one of two kinds, and the kind matters: a **decision**
records why something is the way it is and is not expected to change on its own;
a **record** is an account of work that happened, with the commits and the
numbers, and is expected to be closed rather than extended. A plan that has been
executed stays in the tree as a record, because the acceptance criteria in it are
the only place the shape of that work is written down.

## Start here

| If you want to | Read |
| --- | --- |
| know what the tree is and how to build it | [../README.md](../README.md) |
| pick the tree up from someone else's hands | [13-handover.md](13-handover.md) |
| know the rules a change must obey | [../AGENTS.md](../AGENTS.md) |
| know what is frozen and must not move | [contracts/README.md](contracts/README.md) |
| understand the agent execution layer | [agent-comparison.md](agent-comparison.md), then [agent-gaps.md](agent-gaps.md) |
| plan the next piece of work | [agent-gaps.md](agent-gaps.md) (the ledger of what is missing) |

## Decisions — why it is shaped this way

| Doc | What question it answers, and who reads it | State |
| --- | --- | --- |
| [08 port](08-rust-port.md) | why Rust, which crate owns what, and what the port cost — crate authors | delivered (ADR-0002) |
| [09 tier](09-platform-tier.md) | what the platform tier was missing and what each part is written in — next-wave planner | partially delivered (ADR-0003; P1–P5 in, one item deliberately unbuilt) |
| [11 agent hooks](11-adr-agent-hooks.md) | why there are exactly two rewrite seams and one decision seam, and what each may not do | delivered (ADR-0004, W5 + D wave) |

## Execution records — what was built, and the evidence

| Doc | What question it answers, and who reads it | State |
| --- | --- | --- |
| [10 agent parity](10-agent-parity.md) | the wave plan that closed the agent gaps: scope, contract changes, acceptance per wave | delivered (W1–W6, S1–S5, A–E, G1–G8, H1–H7) |
| [12 subagent port](12-subagent-port.md) | what the deep subagent port delivered (S1–S5), including the out-of-process worker | delivered (S1–S5) |
| [agent gaps](agent-gaps.md) | what DSH has that this tree does not, item by item, with a status and a cost — the ledger to open when planning | delivered; one deliberate non-goal left (`archive-admission` is now **done**) |
| [13 handover](13-handover.md) | where the tree stands, what is deliberately not built, and the traps that already cost hours — whoever picks this up next | delivered (state as of 2026-11-21) |
| [apps/desktop](../apps/desktop/README.md) | the Tauri 2 window around the web UI: how it starts the server, why it is a separate workspace, and the two window-creation defects it cost — desktop shell owner | delivered |

## Comparisons — how this tree relates to DSH

| Doc | What question it answers, and who reads it | State |
| --- | --- | --- |
| [dsh vs hearth](dsh-vs-hearth.md) | how the two codebases actually differ, layer by layer from rc.5 source — design reader | delivered |
| [dsh comparison](dsh-comparison.md) | how fast the installed rc.1 CLI is here, and how the measurement was taken — perf claims, and the source of the numbers quoted above | delivered (measurement record) |
| [agent comparison](agent-comparison.md) | how the two agent loops differ — two independently evolved designs (DSH→forge-go→hearth) | delivered |

## Operations — how it is built, shipped and measured

| Doc | What question it answers, and who reads it | State |
| --- | --- | --- |
| [bench](bench.md) | is the build slower than the baseline, and where the three baselines came from — perf owner | delivered |
| [ci](ci.md) | what CI runs, what each run caught, and what was only rehearsed — workflow editor | delivered |
| [release](release.md) | what an artifact does and does not claim — release builder | delivered |
| [web-ui](web-ui.md) | does a 10k-event transcript hold 60 fps — transcript owner | delivered |
| [office](office-run.md) | does the Office skill actually produce documents — skill editor | delivered |

## Contracts — the frozen surface

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
`partially delivered` on purpose: waves P1–P5 are in, and the one item it
deliberately leaves unbuilt is the pure-Rust document writer, which its §5 says
only becomes worth writing if a single-binary install is ever required.
[ci](ci.md) and [release](release.md) are delivered as documents, and both
now describe runs that actually happened, on hosted runners, with the run ids.
[office](office-run.md) is the same kind of record for the Office skill: what the
shipped instructions produced, what was missing, and what was never verified.
[dsh vs hearth](dsh-vs-hearth.md) states its version boundary up front — it reads
rc.5 source but reuses rc.1 performance numbers, and says so before drawing any
conclusion; [dsh comparison](dsh-comparison.md) is where those numbers and their
method live, which is why the two are separate files rather than one.
[agent comparison](agent-comparison.md) narrows the same rc.5 source down to the
agent execution layer: loop shape, inbox, extension points, stream accounting,
cancellation, subagent, team/goal and compaction. It reads `hearth` as a Rust
port of `forge-go` (which was written by the DSH team), so where the two differ
it is a difference between two independently evolved designs, not a porting bug;
[agent gaps](agent-gaps.md) is the ledger of what DSH has and this tree does not,
rated P0–P2, and is the one to open when planning the next wave. Where this table
and a document disagree, the document is the source of truth and the table is the
bug. [agent parity](10-agent-parity.md) is the execution roadmap that follows the
gap ledger: it fixes the language choice (single Rust kernel, plugin bodies in any
language, TypeScript front-end), splits the work into waves W1–W6, and states each
wave's contract changes, tests and acceptance — **every wave it names is now built**
(W2 turned out to be satisfied by the frozen contract already, so it needed no
code), and the waves that followed it (S1–S5 for subagents, A–E, G1–G8 and H1–H7)
closed the rest of the ledger.
[13 handover](13-handover.md) is a record with an expiry date built in: its counts,
its gate result and its binary size were measured at the handover commit, so a
later reader should re-run `check.ps1` rather than trust the numbers, and the
list of traps is the part that does not expire.

## Keeping this index from rotting

`hearth help` ends by naming this file in its usage text
([crates/cli/src/lib.rs](../crates/cli/src/lib.rs) `USAGE`), and
`cargo run -q -p archcheck --offline` now fails `docs.missing` when a
`docs/<file>.md` named there is not a file on disk. A new document gets a row
here; deleting one means changing that usage line first, in the same change.

The counts in the opening paragraph are the ones that rot quietly, so they are
worth naming: the event corpus grows every time an event type is added, and the
number in this file has been wrong before. `Get-ChildItem tests/conformance/cases/*`
is the check, and `crates/core/tests/conformance.rs` asserts the event count so a
mismatch fails the build rather than sitting here.