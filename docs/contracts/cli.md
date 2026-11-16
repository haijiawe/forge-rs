<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# forge CLI contract

Status: **normative for the commands that exist today**, as implemented by
`cmd/forge` at version `0.0.0-dev`. Anything not in [§2](#2-commands-that-exist-today)
is not part of the CLI.

Everything below was verified by running the built binary; the exact byte
sequences are reproduced from that run. Exit codes are part of the contract: a
reimplementation that prints the right text but returns the wrong code is
non-conforming.

---

## 1. Invocation model

```text
forge <command> [flags]
```

- Exactly one command per invocation. One command has a subcommand:
  `forge session fork` (§2.6). Everything else is a leaf.
- Global flags come after the command (`forge doctor --json`), never before it.
  A command's own flags and its positional arguments may appear in either
  order, because `cmd/forge/flags.go` moves flags ahead of positionals before
  `flag` parses them (§2.3 "Prompt"). `--` still ends flag parsing.
- Output goes to **stdout** unless a rule below says stderr. `forge chat` splits
  the two: the answer is stdout, diagnostics are stderr (§2.3).
- `forge` never prompts for input of its own accord. Two invocations read stdin:
  `forge chat` with no prompt argument, which reads it to EOF (§2.3), and
  `forge chat --approve=prompt`, which reads one line per tool approval
  (§2.3, "Tools and approvals").
  Every other command ignores stdin and never blocks waiting for input.
- Version string: `0.0.0-dev` (injected at build time via `-ldflags`). Commit:
  `unknown`. Both are printed; neither is stable across builds.

### Exit codes

| Code | Name | When |
| --- | --- | --- |
| `0` | success | Command completed. Includes `--version`, `help`, `doctor` where no check has status `fail`, and a `chat` turn that ended `completed`. |
| `1` | failure | `doctor` completed but at least one check reported `fail`; also used when the config path cannot be determined. For `chat`: the turn could not run or ended in `error` (§2.3), plus the three ways a `--resume`/`--fork` can be refused — the session id does not exist, the log has a turn waiting for an approval, or another process holds the session (§2.3 "Resume and fork"). Not a crash. |
| `2` | usage error | Unknown command, unknown flag, flag parse failure, invalid `--session`, `--resume` or `--fork`, `--resume` together with `--fork`, `--resume` together with `--session`, empty prompt, unknown `--approve` value, `--no-tools` together with a non-default `--approve`, `--approve=prompt` with a stdin that is not a terminal, an unknown `--sandbox` value, a fork cut that is not on a record boundary or lands inside an open turn, and `forge web --approve=prompt` (the web approver is `web`, not a terminal). Always accompanied by a message on stderr. |
| `130` | cancelled | `forge chat` interrupted by Ctrl+C. The turn is closed in the log with reason `cancelled` before the process exits (§2.3). |

Any exit code outside this table (including a Go panic's `2` from the runtime) is
a bug, not contract. `forge` does not use exit code `1` for "unknown command".

---

## 2. Commands that exist today

| Command | Aliases | Output | Exit |
| --- | --- | --- | --- |
| `forge --version` | `-v`, `version` | one version line on stdout | `0` |
| `forge chat [flags] [prompt]` | `forge "<prompt>"` | streamed answer on stdout, diagnostics on stderr | `0`, `1`, `2` or `130` |
| `forge web [flags]` | — | three lines on stderr — `serving <baseURL>`, `session=… model=… workspace=… sandbox=… approve=…`, `open <url with token>` — and the UI in the browser it opens | `0`, `1`, `2` or `130` |
| `forge session fork <parentId> [flags]` | — | the new session id (or one JSON object) on stdout, diagnostics on stderr | `0`, `1` or `2` |
| `forge import <old-dir> [flags]` | — | plan or result on stdout, diagnostics on stderr | `0`, `1` or `2` |
| `forge index [flags]` | — | stats, hits or JSON on stdout | `0`, `1` or `2` |
| `forge help` | `-h`, `--help`, no arguments at all | usage text on stdout | `0` |
| `forge doctor` | — | human-readable check report on stdout | `0` or `1` |
| `forge doctor --json` | — | JSON report on stdout | `0` or `1` |

`forge session` has one implemented subcommand today: `fork`. `list`, `show`,
`resume` and `export` are reserved words (§2.6) and exit `2` with a message rather
than pretending to work. `forge resume` is not a command: resuming is
`forge chat --resume <id>` (§2.3), because a resume is a chat turn on an existing
log, not a separate operation.

`--version` is a fast path: it does not touch the config file, the data
directory, the workspace or the network, so it works in a broken environment.

### 2.1 `forge --version`

stdout, exactly one line:

```text
forge 0.0.0-dev (unknown) windows/amd64 go1.27.0
```

Format: `forge {version} ({commit}) {goos}/{goarch} {goVersion}` followed by
`\n`. `{goVersion}` is the Go runtime version the binary was built with
(`runtime.Version()`); a non-Go reimplementation should print its own runtime
version in that slot, or `unknown` if it has none. Nothing is written to stderr.
Exit `0`.

### 2.2 `forge help`

`forge` with no arguments, `forge help`, `forge -h` and `forge --help` all print
the same text to **stdout** and exit `0`. The text is:

```text
forge — an agent harness

usage:
  forge --version              print version (fast path, no config)
  forge chat [flags] [prompt]  run one turn and stream the answer to stdout
  forge "<prompt>"             shorthand for forge chat
  forge web [flags]            serve the browser UI on loopback
  forge session <command>      operate on a session log without running a turn
  forge import <old-dir>       import a session tree from the old harness
  forge index [flags]          rebuild or query the search index
  forge doctor [--json]        check the environment
  forge help                   this message

docs: docs/README.md
```

Exactly: the first line uses an em dash (U+2014), there is a blank line after it,
nine two-space-indented command lines padded so descriptions start in the same
column, a blank line, then the `docs:` line. The block ends with a single `\n`.
Nothing is written to stderr, and no config or filesystem access happens.

`cmd/forge/main_test.go` pins this text byte-for-byte against the constant the
binary prints, so the two cannot drift: adding a command means editing the
constant, this block and that test in one change.

### 2.3 `forge chat [flags] [prompt]`

Runs one agent turn against the workspace and streams the model's answer to
stdout. `forge "<prompt>"` is a shorthand for the same command (§2.5 explains
when a first argument is a prompt instead of a mistyped subcommand).

```text
$ forge chat "summarize internal/session"
The session package owns the append-only event log…
forge chat: model=gpt-4o-mini sandbox=workspace-write tools=7 approve=never
forge chat: session=20260101-120000-a1b2c3
forge chat: log=C:\Users\Administrator\.forge\sessions\20260101-120000-a1b2c3.jsonl
forge chat: turn=1 steps=1 reason=completed usage in=812 out=233 total=1045
```

The stdout/stderr split is contract:

- **stdout** carries the assistant text and nothing else. Fragments are written
  as they arrive, so the answer streams; there is no newline per fragment. If
  the turn produced text that does not end with `\n`, exactly one `\n` is
  appended after the last fragment so the shell prompt starts on a fresh line. A
  turn that failed after emitting partial text keeps that text and gets the
  newline before the diagnosis. Tool output never reaches stdout: a tool's
  `fs_read` result, a `shell` command's stdout and an approval question are
  diagnostics, not answers.
- **stderr** carries every diagnostic, one line each, all prefixed
  `forge chat: `. In order: `model={id} sandbox={mode} tools={n} approve={mode}`,
  `session=`, `log=`, then
  `turn={n} steps={n} reason={r} usage in={n} out={n} total={n}`, then a one-line
  failure diagnosis when the turn failed, then — only when a tool call was
  refused because `--approve=never` left nobody to ask — one hint line naming
  `--approve=always` and `--approve=prompt`. The summary line is printed for
  every turn that started, including failures and Ctrl+C. Approvals asked and
  answered under `--approve=prompt` also go to stderr, each followed by
  `forge chat: approval: allow` or `forge chat: approval: deny`.
- `forge chat "q" > answer.txt` therefore captures only the answer, and
  `2> err.txt` captures only the diagnostics (plus any tool or approval text).

`usage` counts tokens summed over the turn: `in` is prompt tokens, `out` is
completion tokens, `total` is their sum (cache tokens are not included).

#### Prompt

- Positional arguments after the flags are joined with single spaces:
  `forge chat --model m fix the bug` has prompt `fix the bug`. Flags may come
  **before or after** the prompt: `cmd/forge/flags.go` (`reorderFlags`) moves
  flags ahead of the positionals before `flag` parses them, keeping each
  value-flag next to its value and honouring a `--` terminator (everything after
  `--` stays prompt text, even if it starts with `-`). The value-flags it knows
  are `--model`, `--session`, `--resume`, `--fork`, `--system`, `--approve` and
  `--sandbox`; an unknown flag is still an error wherever it appears.
- With no positional argument, stdin is read to EOF and trailing `\r`/`\n` are
  removed: `echo hi | forge chat` and `forge chat < prompt.txt` both work. This
  and the approval questions of `--approve=prompt` are the only paths on which
  `forge` reads stdin.
- An empty or whitespace-only prompt is a usage error: exit `2`, stderr
  `forge chat: empty prompt: pass it as an argument or pipe it on stdin`, and no
  session file is created.
- `-h` / `--help` anywhere before `--` prints this command's usage text
  (`usage: forge chat [flags] [prompt]` …) to stdout and exits `0`.

#### Flags

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `--model` | string | config `model` | Model id sent to the provider as-is. With neither flag nor config: exit `1`, `forge chat: no model configured: …`, before any network call. |
| `--session` | string | generated | Session id, which is also the log file name. Must match `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`; otherwise exit `2`, `forge chat: invalid --session "…": …`, decided before the config, stdin or any file access. An existing id appends to that session's log. |
| `--system` | string | empty | System prompt for this turn; sent as the first model message and not recorded in the log. |
| `--approve` | string enum | `never` | Consent for tool calls the sandbox policy marks as `ask`: `never` (deny, nobody is asked), `always` (allow every call, recorded as `flag:--approve=always`), `prompt` (ask on stderr, read one line from stdin). Any other value is a usage error: exit `2`, `forge chat: invalid --approve "…": use never, always or prompt`. |
| `--no-tools` | bool | `false` | Start the session with no tools at all: the request advertises none ("Tools and approvals", below). Combining it with any `--approve` value other than `never` is a usage error, exit `2`. |
| `--json` | bool | `false` | Write one report object on stdout instead of streaming (below). Diagnostics still go to stderr. |
| `--resume <id>` | string | — | Continue the existing session `<id>` instead of starting a new one: the next turn number is one greater than the highest turn in the log, and the model sees the prior conversation. Refused (exit `1`) for an unknown id, a log waiting for an approval, or a session another process is writing. Mutually exclusive with `--fork` and `--session`. |
| `--fork <id>` | string | — | Copy the session `<id>` up to its last `turn/end` into a new session and run the turn there; the parent log is never modified. `--session` names the copy (default: a generated id). Mutually exclusive with `--resume`. |
| `--sandbox` | string enum | config `sandbox` | Containment mode for this process: `read-only`, `workspace-write` or `full-access`. Any other value is a usage error: exit `2`, `forge chat: invalid --sandbox "…": use read-only, workspace-write or full-access`, decided before any file access. A mode this host cannot enforce exits `1` before the turn with `sandbox: mode "…" cannot be enforced on this host: …`. A mode that is only partly enforced prints one warning line (§2.7). |
| `--no-compact` | bool | `false` | Skip the compaction pass that runs before the turn (§2.3 "Compaction"). |

Both `-flag` and `--flag` spellings are accepted (`flag` package). An undefined
flag prints the `flag` package error and a short flag listing to stderr and exits
`2`.

The generated session id is `YYYYMMDD-HHMMSS-xxxxxx`: local time formatted
`20060102-150405`, a hyphen, then six lowercase hex digits from three random
bytes. Two runs in the same second differ. The log path is
`<dataDir>/sessions/<id>.jsonl`, where `<dataDir>` comes from the config
(default: `$FORGE_HOME`, else `~/.forge`) — the path printed on stderr.

Every turn appends to that log, which follows [`events-v1`](events-v1.md):
`turn/start` … `turn/end` with `reason` equal to the value reported on stderr,
and the assistant text in `assistant/message`. The log is append-only;
`forge chat` never rewrites or deletes it.

#### Resume and fork

Both flags start a turn against a log that already exists. Neither needs a new
code path in the agent: a turn always rebuilds its history from the log, so
continuing is the ordinary path with an older file.

`--resume <id>` opens `<dataDir>/sessions/<id>.jsonl` and continues it. The first
turn appended is the highest turn in the log plus one, and the request carries the
prior conversation. Recovery is part of the contract, because a killed process is
the normal reason to resume:

- A torn final record — a crash in the middle of `write` — is dropped: the file is
  truncated back to its last complete record, and stderr says
  `forge chat: recovered session <id>: dropped <n> bytes of a torn final record`.
  Every byte before that point is untouched.
- A `turn/start` with no `turn/end` (the process died mid-turn) is closed by
  appending a `turn/end` with `reason="cancelled"` and `cancelledBy="cli:recovery"`
  — a corrective event, never a rewrite. The next turn is then the one after it.
  stderr says
  `forge chat: recovered session <id>: closed turn <n>, which a dead process left open`.
  Nothing else is invented: an unanswered `step/start` stays unanswered, and its
  assistant text, if any, is already in the log.

#### Compaction

Before the turn starts, `forge chat` runs one compaction pass over the log
(`session.CompactLog`) when a provider is configured and the log holds at least
200 events (`compactMinEvents`; below that floor nothing is planned). It appends
at most one `compaction/summary` event ([`events-v1`](events-v1.md) §5.1) and
never rewrites a record. When the log has a summary span afterwards, stderr says
`forge chat: compacted <n> events into a summary`, or
`forge chat: compaction skipped: <err>` when the pass returned an error. A skip
is not a turn failure and does not change the exit code. `--no-compact` skips
the pass entirely, and so does a run with no provider configured. `forge web`
does not compact.

`--resume` refuses rather than interleaves two writers into one log. Each of these
exits `1` and creates, truncates or appends nothing:

| Situation | stderr | What the operator does |
| --- | --- | --- |
| `<id>.jsonl` does not exist | `forge chat: no such session "<id>": <path> does not exist` | Check the id, or start a new session. |
| the newest turn is waiting for an approval | ``forge chat: session "<id>" is waiting for approval "<approvalId>": answer it in `forge web`, or continue from a copy that stops before that turn with `forge chat --fork <id>` `` | Answer it in the web UI, or fork past it. |
| `<id>.lock` names a live pid | `forge chat: session is being written by another process: pid <n> is writing session "<id>": let that turn finish (or stop that process), then retry` | Wait for that turn, or stop that process. |

The lock is a sidecar file `<dataDir>/sessions/<id>.lock` holding the writer's
pid, created with `O_CREATE|O_EXCL` and removed when the turn ends. A lock whose
pid is dead, unparsable or this very process is stale and is taken over. It is
advisory: it covers `forge chat --resume` against another `forge chat --resume`,
not against `forge web`, whose turns live in the server's memory — a live web turn
looks like a stale lock to a separate CLI process. Nothing else in the data
directory is touched, and the lock is not part of the log.

`--fork <id>` writes a **new** session whose log is a verbatim copy of the
parent's events up to a cut, then runs the turn there. The cut defaults to the
parent's last `turn/end`, so a fork never starts mid-turn and never inherits an
open turn. Copied records keep their envelope and payloads; only the `session`
field is rewritten to the new id, `seq` is renumbered from `1`, and the head
record gains `forkedFrom`, `forkedAtSeq` and `forkedAt` ([`events-v1`](events-v1.md)
§4.1). The parent log is read-only for the whole operation: its bytes are
identical afterwards, and a fork never truncates the parent's torn tail.

`--fork` refuses (exit `1`, nothing created) a parent that does not exist or a
`--session` id whose log already exists — a copy never overwrites a log, so pick
another id. `forge chat --fork` also prints
`forge chat: forked <parent> at seq <n> into session <new> (<events> events)` on
stderr before the turn's own lines.

#### Tools and approvals

`forge chat` builds the tool pipeline and advertises it on every model call. The
workspace is the process's current directory, resolved with `os.Getwd` before
the session log is opened; if that fails the command exits `1` with
`forge chat: working directory: …` rather than falling back to an empty path.
The same value is passed to the agent (recorded in `session/start`) and to the
tool executor.

Seven tools are registered unconditionally. The pipeline then adds whatever the
M5 bridges contribute, so `tools={n}` on stderr — the count of everything in the
registry — is at least seven and grows with configuration:

- one tool per MCP tool, named `mcp__<server>__<tool>` with `_` escaped to `__`
  and `.` to `_dot_` (§5 of [`m5-surface`](m5-surface.md)), policy `ask` in every
  mode;
- `skill` when `skills.roots` yields at least one usable `SKILL.md`;
- `subagent` when `subagent.maxDepth` is greater than zero.

`--json` `tools` is the sorted name list of everything advertised, bridges
included. A misconfigured MCP server or skill root is never fatal: that bridge
contributes nothing and writes one warning to stderr. The table below is the
policy for the seven unconditional tools.

| Tool | Sandbox policy `read-only` | `workspace-write` | `full-access` |
| --- | --- | --- | --- |
| `fs_read`, `fs_glob`, `fs_grep` | allow | allow | allow |
| `fs_write`, `fs_edit` | deny | allow inside the workspace | allow |
| `todo_write` | deny | allow | allow |
| `shell` | deny | **ask** | allow |
| anything not listed | deny | deny | allow |

The mode comes from `sandbox` in `config.json` (`read-only`, `workspace-write`,
`full-access`; default `workspace-write`), and `--sandbox` overrides it for one
invocation. It is reported on stderr as `sandbox={mode}`.

The model may ask for several tools in one step; the agent executes them inside
the turn and appends `tool/call` and `tool/result` events. A tool that failed,
was denied, or timed out is **data**: the result carries the reason, the model
sees it, and the turn continues. It does not change the exit code — only the
turn ending `error` does (§2.3 exit codes).

A policy decision of `ask` is put to an *approver*. What the approver is depends
on `--approve`:

| `--approve` | Approver | Effect |
| --- | --- | --- |
| `never` (default) | none | The ask is refused without asking anybody. `tool/result` has `ok=false`, `code="no_approver"`, and **no** `approval/request` or `approval/decision` is recorded — nobody was asked, so there is nothing to record. One stderr hint line (`--approve=always` / `--approve=prompt`) is printed after the summary line. |
| `always` | allows everything | `approval/request` then `approval/decision` with `decidedBy="flag:--approve=always"` and `decision="allow"`. |
| `prompt` | the human | One question per ask on stderr, `forge chat: approve tool={name} in {workspace}? args=… [y/N] `, then one line is read from stdin: `y` or `yes` (case-insensitive) allows, anything else — including EOF — denies. `decidedBy="user"`. A refusal is `code="denied_refused"`. |

`--approve=prompt` requires a terminal on stdin. Under a pipe, a redirection, or
`go test` it is a usage error: exit `2`, stderr carries

```text
forge chat: --approve=prompt needs a terminal on stdin: use --approve=always to allow every tool call, or run forge chat in a terminal
```

and no session file is created. It never blocks on a pipe. Because `forge chat`
with no prompt argument reads stdin to EOF, `--approve=prompt` needs the prompt as
an argument; otherwise the approvals see EOF and deny.

`--no-tools` registers nothing: the request advertises no tools (`--json`
`tools` is `[]`) and a model that asks for one anyway gets a recorded refusal.
It cannot be combined with `--approve=always` or `--approve=prompt` — with no
tools there is nothing to approve — and that combination is a usage error.

#### Environment

| Variable | Effect |
| --- | --- |
| `FORGE_BASE_URL` | Provider base URL. Default `https://api.openai.com/v1`. |
| `FORGE_API_KEY` | Provider API key, sent as `Authorization: Bearer …`. Unset means no `Authorization` header; it is not a usage error. |
| `FORGE_HOME` | Forge home, as for `doctor`; the data directory defaults to it, so it also decides where `sessions/` lands. |

Endpoint and key precedence is: explicit adapter option, then the environment
variable, then the default. The CLI passes no explicit option, so these
variables are the only way to point `forge chat` at a non-default endpoint.

#### `--json`

One JSON object on stdout, compact (no indentation), followed by one `\n`:

```json
{"turn":1,"reason":"completed","steps":2,"text":"Hello, forge","usage":{"inputTokens":7,"outputTokens":3},"events":11,"tools":["fs_edit","fs_glob","fs_grep","fs_read","fs_write","shell","todo_write"],"approvals":[{"callId":"call-1","tool":"shell","decision":"allow"}]}
```

| Member | Type | Meaning |
| --- | --- | --- |
| `turn` | integer | Turn number within the session; `1` for a new session. |
| `reason` | string | The `turn/end` reason: `completed`, `error` or `cancelled`. |
| `steps` | integer | Model calls attempted; a call that failed counts as one. |
| `text` | string | Assistant text of the turn; empty when the turn produced none. |
| `usage` | object | `inputTokens` and `outputTokens`, summed over the turn; `cacheReadTokens` / `cacheWriteTokens` appear only when non-zero. |
| `events` | integer | Events this invocation appended to the log. |
| `tools` | array of string | Names the request advertised, sorted. `[]` under `--no-tools`, never `null`. |
| `approvals` | array of object | Approvals decided during this turn, in event order; `[]` when none, never `null`. |
| `approvals[].callId` | string | The `tool/call` id the approval answered. |
| `approvals[].tool` | string | Tool name, e.g. `shell`. |
| `approvals[].decision` | string enum | `allow` or `deny`, as recorded in `approval/decision`. |

`tools` is what was *advertised*, not what ran; a model may use none of them.
`approvals` lists only decisions taken in this turn — an ask refused for lack of
an approver (`--approve=never`) is not one, because nobody was asked. The
deciding party (`decidedBy`) stays in the event log; it is not repeated here.

The object is written on **every** path that started a turn — including
`reason` `error` and `cancelled` — and it is the only thing on stdout in this
mode (the answer is not streamed as well). The exit code still follows the table
below.

#### Exit codes

| Code | When |
| --- | --- |
| `0` | The turn ended `completed`. A `max_tokens` stop counts as completed. Tools denied by policy, refused by an approver, failed or timed out are data the model reads, not a failed turn: the turn still ends `completed` and the exit code is `0`. |
| `1` | The turn could not run or ended `error`: the config cannot be read, no model is configured, the working directory cannot be resolved, the provider/transport fails, the stream breaks, or the model asked for tools that do not exist (which includes asking at all under `--no-tools`). A one-line diagnosis goes to stderr. |
| `2` | Usage error: unknown flag, invalid `--session`, empty prompt, unknown `--approve` value, `--no-tools` combined with `--approve=always`/`--approve=prompt`, or `--approve=prompt` with a stdin that is not a terminal. No session file is created. |
| `130` | Ctrl+C: the first signal cancels the turn, closes it in the log with reason `cancelled`, prints the summary line and exits `130`; a second Ctrl+C exits immediately without unwinding. |

### 2.4 `forge doctor [--json]`

Runs environment checks and reports them. The checks are read-only except for the
data-directory probe, which creates and deletes `.write-probe` inside the data
directory.

Flags:

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `-json` / `--json` | bool | `false` | Emit the JSON report instead of the text report. Both spellings are accepted (`flag` package). |

Checks, in this order (all ten always run; order and names are contract):

| # | Name | Verifies |
| --- | --- | --- |
| 1 | `config` | A config file is found and parses; otherwise defaults apply. |
| 2 | `data dir` | The data directory exists (or can be created) and is writable. |
| 3 | `workspace` | The current workspace exists; notes whether it is a git repo. |
| 4 | `git` | A `git` executable is on `PATH`. |
| 5 | `sandbox` | The verifier constructs the real enforcer for the configured mode and reports its capabilities. `skip` under `full-access` (nothing is enforced by design) or an invalid spec; `fail` when the host cannot enforce the mode; `warn` when the mode is only partly enforced. |
| 6 | `disk` | Free space on the data directory's volume: `fail` below 512 MiB, `warn` below 2 GiB. |
| 7 | `model` | One `HEAD` request against the provider base URL, 2 s timeout. Never `fail`: `warn` on 401/403 (pointing at `FORGE_API_KEY`), `warn` on transport trouble, `skip` when no model is configured. |
| 8 | `mcp` | The MCP configuration only — no server is spawned. `skip` with no server configured, `warn` when entries were dropped or duplicated (the reasons are in the hint), `ok` listing the accepted names. |
| 9 | `proxy` | HTTP(S) proxy environment variables, if any. |
| 10 | `pty` | PTY availability. `skip` on windows/linux/darwin with the detail `shell runs on pipes, not a pty: interactive programs will not render` (the shell tool deliberately uses pipes; the M0 spike proved ConPTY works but no PTY is wired into the product); `warn` on any other platform. |

Per-check status vocabulary: `ok`, `warn`, `fail`, `skip`. **Only `fail` affects
the exit code**; `warn` and `skip` do not.

#### Text output

```text
forge 0.0.0-dev (unknown)  windows/amd64  go1.27.0
config     C:\Users\Administrator\.forge\config.json
workspace  D:\魔改版Agent框架\forge-v2

OK    config     using defaults (no config file)
FAIL  data dir   config: data dir C:\Users\Administrator\.forge is not writable: open C:\Users\Administrator\.forge\.write-probe: Access is denied.
                 -> pick a writable dataDir in C:\Users\Administrator\.forge\config.json
OK    workspace  D:\魔改版Agent框架\forge-v2 (git repo)
OK    git        E:\Tools\PortableGit\cmd\git.exe
WARN  sandbox    windows-job-object: windows job object: process tree (KILL_ON_JOB_CLOSE), process memory 1 GiB, job memory 2 GiB, active processes 64 — filesystem and network are NOT confined
                 -> the enforcer installed, but filesystem writes are not confined on this host; the detail is its own account of what it does enforce
OK    disk       38.5 GiB free of 149.1 GiB on the volume holding C:\Users\Administrator\.forge
SKIP  model      no model configured
SKIP  mcp        no MCP servers configured
OK    proxy      none set
SKIP  pty        shell runs on pipes, not a pty: interactive programs will not render
                 -> ConPTY/pty is available on this platform and can be wired when a terminal is needed

10 checks in 22 ms
```

Structure, line by line:

1. `forge {version} ({commit})  {goos}/{goarch}  {goVersion}` — two spaces
   between the three groups.
2. `config     {configPath}` — the label is left-justified in an 11-column field,
   then the path. When no config file exists the path is the one that *would* be
   used, not empty.
3. `workspace  {workspacePath}` — same 11-column label field.
4. A blank line.
5. One row per check: `{STATUS}` padded to the widest status, at least two spaces,
   `{name}` padded to the widest name, at least two spaces, `{detail}`. `STATUS`
   is the status uppercased (`OK`, `WARN`, `FAIL`, `SKIP`). Column widths are
   computed across all rows of the table (a tabwriter with min width 0, padding
   2, pad char space), so they shift with content.
6. For checks that carry a hint, one extra row: the status column empty and the
   name column empty, the detail column holding `-> {hint}`. The hint is
   indented to the detail column of the table.
7. A blank line.
8. `{n} checks in {d} ms` — `n` is the number of checks run (10 today), `d` is the
   elapsed milliseconds. Both are integers. The timing varies per run and is not
   contract.

The report is written to stdout. Exit code: `1` if any check has status `fail`,
else `0`.

#### JSON output (`--json`)

A single JSON object, 2-space indented (Go `json.Encoder` with
`SetIndent("", "  ")`), followed by one `\n`:

```json
{
  "version": "0.0.0-dev (unknown)",
  "os": "windows",
  "arch": "amd64",
  "goVersion": "go1.27.0",
  "configPath": "C:\\Users\\Administrator\\.forge\\config.json",
  "workspace": "D:\\魔改版Agent框架\\forge-v2",
  "durationMs": 16,
  "checks": [
    {
      "name": "config",
      "status": "ok",
      "detail": "using defaults (no config file)"
    },
    {
      "name": "data dir",
      "status": "fail",
      "detail": "config: data dir C:\\Users\\Administrator\\.forge is not writable: open C:\\Users\\Administrator\\.forge\\.write-probe: Access is denied.",
      "hint": "pick a writable dataDir in C:\\Users\\Administrator\\.forge\\config.json"
    }
  ]
}
```

(Excerpt: a real run has ten entries in `checks`.)

Schema:

| Member | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `version` | string | always | `Version` composed with the commit, exactly `"0.0.0-dev (unknown)"` for this build. Note this is the whole version line's first group, not the bare `0.0.0-dev` that `--version` prints in front of it. |
| `os` | string | always | `runtime.GOOS`. |
| `arch` | string | always | `runtime.GOARCH`. |
| `goVersion` | string | always | Build toolchain version. |
| `configPath` | string | always | Config file path used (existing or default). |
| `workspace` | string | always | Workspace directory that was checked. |
| `durationMs` | integer | always | Elapsed milliseconds; not stable. |
| `checks` | array of object | always | Ten entries, in the order of the table above. |
| `checks[].name` | string | always | Check name, e.g. `data dir`. |
| `checks[].status` | string enum | always | `ok` \| `warn` \| `fail` \| `skip`. |
| `checks[].detail` | string | always | One-line explanation; may be empty. |
| `checks[].hint` | string | **omitted when empty** | What to try next. |

Key order in the output is struct order (as shown) but consumers MUST NOT depend
on it. `hint` is present only when non-empty. `checks` is an array, never `null`.
Extra members may be added in a compatible release; consumers must ignore unknown
members. Exit code is the same as the text mode: `1` iff some check has status
`fail`.

#### Environment

| Variable | Effect |
| --- | --- |
| `FORGE_HOME` | Overrides the forge home directory. Config path becomes `$FORGE_HOME/config.json`; the default data directory is `$FORGE_HOME`. |
| *(unset)* | Home is `~/.forge`; config path `~/.forge/config.json`. |

If the config path cannot be determined (no home directory at all), `doctor`
exits `1`. The filesystem sandbox this project is developed under allows writes
inside the repo only, so the default home directory reports
`data dir … is not writable` and `doctor` exits `1` — that is correct behaviour,
not a bug.

#### Usage errors around `doctor`

An undefined flag is a usage error: exit `2`, and stderr carries the Go `flag`
package message:

```text
flag provided but not defined: -nope
Usage of doctor:
  -json
    	machine-readable output
```

`doctor` ignores positional arguments after its flags today: `forge doctor
junk` is **not** a usage error (never exit `2`) and the exit code follows the
checks like any other `doctor` run — verified `1` with an unwritable data
directory and `0` with a writable one. Do not rely on that: it is unspecified
and may become a usage error.

### 2.5 Unknown commands

```text
$ forge nope
forge: unknown command "nope"

<usage text>
```

The `forge: unknown command "nope"` line, a blank line and the full usage text go
to **stderr**, and the exit code is `2`. The usage text is byte-identical to
`forge help` (which goes to stdout). The command name is quoted with `%q`
semantics (JSON-style escaping for non-printable characters).

Only a *single bare ASCII word* is treated as a mistyped subcommand this way.
Anything containing whitespace or a non-ASCII character, or starting with `-`,
is sent to `forge chat` instead (§2.3), so `forge fix the bug` and
`forge 总结这个仓库` are chat turns while `forge fix` is exit `2`. A one-word
prompt therefore needs the explicit form: `forge chat fix`.

---

### 2.6 `forge session fork <parentId> [flags]`

Copies a session into a new one without running a turn, for scripts that want a
branch point and will run the turn later (or through `forge chat --resume`).
It is the same operation as `forge chat --fork`, minus the turn.

```text
forge session fork <parentId> [--at <seq>] [--id <newId>] [--json]
```

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `--at <seq>` | uint | parent's last `turn/end` | Last record to copy, inclusive. Must name an existing record, and no turn may be open at that point in the parent (`0` is rejected: seqs are dense from `1`). |
| `--id <newId>` | string | generated | Id for the copy. Refused if `<newId>.jsonl` already exists. |
| `--json` | bool | `false` | Print one JSON object instead of the bare id. |
| `-h`, `--help` | flag | — | Print usage on stdout, exit `0`. |

- **stdout**: the new session id on its own line, or with `--json` exactly one
  object `{"parent":"…","session":"…","atSeq":n,"events":n,"bytes":n}` — `atSeq`
  is the cut in the parent, `events` the number of copied records, `bytes` the
  size of the new log. Nothing else is written to stdout.
- **stderr**: one summary line
  `forge session fork: forked <parent> at seq <n> into <new> (<events> events, <bytes> bytes)`,
  or one diagnosis line, always prefixed `forge session fork: `.
- **Exit `0`**: the new log was written and synced. **`1`**: the parent does not
  exist, the target id is taken, or the write failed — in every case the target
  log is not left behind. **`2`**: usage error — no parent or more than one,
  unknown flag, invalid id, `--at` outside the log, `--at` inside an open turn.

The parent is only read. It is not locked, not truncated and not rewritten: two
forks of one parent, or a fork during another process's turn, all see the same
bytes. The copy follows [`events-v1`](events-v1.md) §4.1: identical records,
`session` rewritten, `seq` renumbered from `1`, and a head record that names the
parent.

```text
$ forge session fork 20260101-120000-a1b2c3 --id branch-1
branch-1
forge session fork: forked 20260101-120000-a1b2c3 at seq 12 into branch-1 (12 events, 3072 bytes)
```

---

### 2.7 `forge web [flags]`

Starts the loopback HTTP + WebSocket server specified by
[`api.md`](api.md) and serves the browser UI from the same origin: `/api/*` and
`/healthz` are the JSON API, every other path is the UI's app shell. It blocks
until Ctrl+C and then shuts down with a 5 s grace period for in-flight work.

```text
forge web [--addr host:port] [--model <name>] [--workspace <dir>]
          [--approve web|never|always] [--sandbox <mode>] [--no-browser]
```

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `--addr` | string | `127.0.0.1:8420` | Listen address. Protocol 1 is loopback-only: a non-loopback host is refused with exit `1` (`server: refusing to bind "<addr>": protocol 1 is loopback only`). Port `0` picks a free port and prints it. |
| `--model` | string | config `model` | Model id for the turns this server runs. With neither flag nor config: exit `1`, `forge web: no model configured: pass --model <name> or set "model" in config.json`. |
| `--workspace` | string | current directory | Workspace for the session the server opens. Must be an existing directory; otherwise exit `1`. |
| `--approve` | string enum | `web` | Who answers tool asks: `web` (in the browser, `decidedBy` is `user`), `never` (deny, recorded as `no_approver` on the tool result), `always` (allow, `decidedBy="flag:--approve=always"`). `prompt` is a usage error — exit `2`, `forge web: --approve=prompt reads stdin, which `forge web` does not ask on: use --approve=web to answer in the browser`; any other value is exit `2`, `forge web: unknown --approve value "…": use web, never or always`. |
| `--sandbox` | string enum | config `sandbox` | As for `chat` (§2.3): `read-only`, `workspace-write` or `full-access`; an unknown value exits `2` and an unenforceable mode exits `1`, both before the listener opens. |
| `--no-browser` | bool | `false` | Print the URL instead of opening a browser. |
| `-h`, `--help` | flag | — | Print usage on stdout, exit `0`. |

`forge web` takes **no positional argument**: anything left over is a usage error,
exit `2`, `forge web: unexpected argument "<arg>": the command takes flags only`.

- **stdout**: nothing.
- **stderr**, in this order, once the listener is up and one session exists:
  1. `forge web: serving <baseURL>`
  2. `forge web: session=<id> model=<model> workspace=<workspace> sandbox=<mode> approve=<mode>`
  3. `forge web: open <url>` — the URL carries the bearer token as a query
     parameter in front of the hash route
     (`<baseURL>/?token=<token>#/s/<sessionId>`), so it is a credential and is
     printed once.
- **stderr**, only on failure: `forge web: <message>`, single line, prefixed.
- **Exit `0`**: the listener closed cleanly (one Ctrl+C). **`1`**: the server could
  not start — config, workspace, model, sandbox, token file, bind, or the
  end-to-end session call. **`2`**: usage error. **`130`**: a second Ctrl+C while
  shutting down; the process exits without waiting for the grace period.

Start-up opens (or reuses) exactly one session for the workspace through the same
HTTP API the browser uses, and it reuses an existing session whose workspace
matches and whose status is `idle` or `running`; otherwise it creates one. The
command therefore fails before printing the URL if routing, auth or the token file
is broken. Closing a browser tab does not end anything: the server runs until it
is interrupted, one turn per accepted message, and it does **not** compact the
event log (§2.3 "Compaction" is `forge chat`-only).

---

## 3. Reserved, not implemented

These names are **reserved** so that a future release can add them without
breaking scripts. They are **not implemented today**: invoking any of them is an
unknown command, i.e. exit `2` with `forge: unknown command "..."` on stderr.
Do not write tooling against them yet.

| Reserved command | Planned milestone | Purpose |
| --- | --- | --- |
| `forge session list` | M4 | List sessions in the data directory. |
| `forge session show <id>` | M4 | Projection of one session. |
| `forge session export <id>` | M4 | Dump the event log / a transcript. |
| `forge config get` / `forge config set` | M6 | Read and write config keys. |
| `forge mcp list` / `forge mcp add` | M5 | Manage MCP servers. |
| `forge doctor --fix` | M6 | Apply safe repairs for failing checks. |

`forge session` is no longer wholly reserved: `forge session fork <parentId>` is
implemented (§2.6). `forge session resume <id>` is covered by
`forge chat --resume <id>` (§2.3), so it is not planned as a separate subcommand.
`forge "<prompt>"` is no longer reserved either: like `forge chat`, it shipped in
M1 (§2.3), including the bare-prompt rule above.

There is deliberately **no** `forge serve`: M3 shipped the local HTTP + WebSocket
server as `forge web` (§2.7), which also serves the UI from the same origin, so a
separate serve-only command is not planned and the name stays an unknown command.

Also reserved but currently a script, not a subcommand: `go run ./scripts/bench`
(performance harness). It may become `forge bench`.

---

## 4. Compatibility notes for implementers

1. **Exit codes are the interface.** Scripts branch on `2` (usage), `1`
   (environment or turn failure), `0` (success) and `130` (interrupted `chat`).
   Keep these stable.
2. **stdout is for machines as well as humans.** `--version` prints one line and
   nothing else. `--json` prints one JSON document and nothing else. Diagnostics,
   including usage errors, go to stderr. `forge chat` streams human-readable
   text; pass `--json` when the answer must be machine-parsed.
3. **No colour, no ANSI, no progress spinners** in current output. Any future
   spinner must not appear when stdout is not a terminal, and must never pollute
   `--json`.
4. **`doctor` is not a liveness check for the harness.** It reports on the
   environment and always finishes in bounded time; `git` and the data-directory
   probe may touch the filesystem.
5. `--version` must not depend on config parsing: a broken config must not change
   the version line or its exit code.
6. A new subcommand is not done until this file lists it with its exit codes
   (see `AGENTS.md`, "New subcommand").

---

## 5. `forge import <old-dir>`

Imports one previous-generation session into this build's data directory as a
new v1 event log. The source is **read-only**: nothing under `<old-dir>` is
created, modified or deleted. The new log becomes the session's only truth
(law 1); the old files stay behind as evidence, not as a second source.

`<old-dir>` may be a single old session directory (`session.jsonl` or
`session.v<N>.jsonl`, N = 1..4) or a whole old data root, in which case the
newest committed generation is imported and the others are reported as a
warning. A compressed old log (`.jsonl.zstd`) is refused, not silently skipped.

### Usage

```text
forge import <old-dir> [flags]
```

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `--dry-run` | bool | `false` | Print the plan and write nothing — not the log, not the data directory. |
| `--session <id>` | string | a fresh timestamped id | Id of the imported session; must match `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`, the same rule `forge chat --resume` uses. |
| `--data-dir <dir>` | string | the forge home's configured `dataDir` | Data directory to write into; the log lands at `<dir>/sessions/<id>.jsonl`. |
| `--json` | bool | `false` | Print one JSON document on stdout instead of the human report. |
| `-h`, `--help` | flag | — | Print usage on stdout, exit 0. |

Flags may appear before or after `<old-dir>`.

### stdout / stderr

- **stdout, success**: the new session id on line 1, the written log path on
  line 2, nothing else. That two-line answer is the machine-readable contract;
  a script takes line 1 as the id.
- **stdout, `--dry-run`**: the plan — one `label  value` row per line
  (`source`, `format`, `session`, `events`, `log`, `dropped`, `warnings`), then
  every dropped record and every warning as a two-space-indented `- ` item.
- **stderr**: every diagnostic, prefixed `forge import: `, one line each. On
  success it still reports the event count, the dropped records and the
  warnings, so a lossy import never looks complete. Empty stderr is not
  promised on success; empty stdout is.
- **`--json`**: stdout becomes one 2-space-indented document — dry run:
  `source`, `format`, `sessionId`, `events`, `log`, `dropped`, `warnings`;
  import: `sessionId`, `log`, `events`, `dropped`, `warnings`. Empty lists are
  `[]`, never `null`, and the dropped/warnings diagnostics still go to stderr.

Dry run (the last line is stderr):

```text
$ forge import D:\old\.forge\sessions\20260101-120000 --dry-run
source    D:\old\.forge\sessions\20260101-120000
format    forge-session-v4
session   20260101-120000-a1b2c3
events    5
log       C:\Users\Administrator\.forge\sessions\20260101-120000-a1b2c3.jsonl
dropped   1
warnings  1
  - legacy/thing: no v1 event type for this old record
  - session D:\old\.forge\sessions\20260101-120000 holds 2 committed generations (v0,v4); importing the newest, v4
forge import: dry run, nothing was written (drop --dry-run to import)
```

### Exit codes

| Code | When |
| --- | --- |
| `0` | The plan was printed (`--dry-run`) or the log was written. |
| `1` | `<old-dir>` is missing or is not an old session directory; the destination id already exists (**nothing is written** — a log is append-only history, so an import does not overwrite or rename); no data directory could be resolved; the write failed. |
| `2` | Usage error: unknown flag, no positional argument or more than one, invalid `--session`. Usage goes to stderr. |

### Example

```text
$ forge import D:\old\.forge\sessions\20260101-120000 --session old-1
old-1
C:\Users\Administrator\.forge\sessions\old-1.jsonl
forge import: imported 5 events from D:\old\.forge\sessions\20260101-120000 into session old-1
forge import: dropped: legacy/thing: no v1 event type for this old record
```

The old format is wider than v1. What cannot be mapped is reported on stderr,
never invented into the log and never silently discarded.

---

## 6. `forge index [flags]`

Builds, refreshes or reads the SQLite session index under the forge home. The
index is derived data: `--rebuild` drops it and re-derives every session from
`<dataDir>/sessions/*.jsonl`. It can be deleted at any time and rebuilt — the
logs stay the only truth (law 1), and nothing here writes to a log.

### Usage

```text
forge index [--rebuild] [--stats] [--search TEXT] [--json] [--db <path>]
```

| Flag | Type | Default | Meaning |
| --- | --- | --- | --- |
| `--rebuild` | bool | `false` | Drop the index and re-derive it from the logs. The only invocation that writes. |
| `--stats` | bool | `false` | Print the totals only (sessions, events, bytes, workspaces), no session list. |
| `--search <text>` | string | — | Search the indexed events. Words are ANDed and matched by prefix; at most 50 hits are printed (the API caps at 200). An **empty query is not an error**: it matches nothing and exits 0. |
| `--json` | bool | `false` | Machine-readable output: one JSON document (or array for `--search`), 2-space indent, keys in struct order, no trailing spaces. |
| `--db <path>` | string | `<dataDir>/index.db` | Index file to use. Its parent directory is the data directory `--rebuild` walks, so this is also how a second data directory is indexed. |
| `-h`, `--help` | flag | — | Print usage on stdout, exit 0. |

`--stats` and `--search` are mutually exclusive (exit `2`).

### Modes

| Invocation | Effect | `mode` printed |
| --- | --- | --- |
| `forge index` | Read-only: totals plus the first 20 sessions. Writes nothing. | `read-only` |
| `forge index --rebuild` | Rebuild, then print the same summary. | `rebuilt` |
| `forge index --stats` | Totals only. | — |
| `forge index --search TEXT` | Hits only. | — |

Without `--rebuild`, a missing index is a failure, not an empty answer: exit
`1` with a one-line hint to run `forge index --rebuild`. Asking about a data
directory must never create an index that claims it holds no sessions.

### stdout / stderr

- **stdout**: the report. Summary and stats are a `label  value` block (the
  summary adds a blank line and an `id  updated  status  title` table);
  `--search` prints `sessionId:seq  [type]  snippet` per hit; `--json` prints
  the document instead. Columns use a tabwriter: a two-space gap, no line ends
  in padding spaces.
- **stderr**: diagnostics only, prefixed `forge index: `. Nothing else is
  written there, so a clean run has empty stderr.

```text
$ forge index --rebuild
index       C:\Users\Administrator\.forge\index.db
mode        rebuilt
sessions    3
events      7
bytes       916
workspaces  2

id  updated               status  title
s1  2026-07-06T15:34:00Z  idle    hello alpha
s2  2026-07-06T15:34:00Z  idle    gamma delta
s3  2026-07-06T15:34:00Z  idle    hello again alpha
```

`bytes` is the total size of the logs covered by the last rebuild; after new
logs are appended it is stale until the next `--rebuild`, which is exactly why
the default mode reports its `mode`.

### Exit codes

| Code | When |
| --- | --- |
| `0` | The report was printed. |
| `1` | There is no index at the path and `--rebuild` was not given; no data directory could be resolved; SQLite failed to open, read or rebuild. |
| `2` | Usage error: unknown flag, a positional argument, `--stats` together with `--search`. Usage goes to stderr. |

### Example

```text
$ forge index --search hello --json
[
  {
    "sessionId": "s1",
    "seq": 2,
    "turn": 0,
    "type": "user/message",
    "snippet": "hello alpha"
  },
  {
    "sessionId": "s3",
    "seq": 2,
    "turn": 0,
    "type": "user/message",
    "snippet": "hello again alpha"
  }
]
```

A search that matches nothing prints nothing (text) or `[]` (JSON) and still
exits `0`: an empty result is an answer, not a failure.