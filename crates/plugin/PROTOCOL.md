# hearth plugin protocol, version 1

A plugin is a child process that speaks newline-delimited JSON-RPC 2.0 over stdin and
stdout. One JSON object per line, UTF-8, terminated by `\n`. No pretty-printing, no length
prefixes, no batching, no `Content-Length` headers. A trailing `\r` is tolerated and
stripped, so a Windows editor will not break a plugin.

stdout carries protocol frames **and nothing else**. A `print`/`console.log`/`println!` that
reaches stdout is a malformed frame; diagnostics go to stderr, which the host captures and
logs but never parses. This is the single most common way a working plugin fails, so it is
the first rule in this document.

This document is the contract. A plugin written from this file alone must work, and the
host must not require anything this file does not state. The reference plugin under
`tests/reference-node/` was written from it; if the two disagree, one of them is a bug.

## Why a process, and why JSON-RPC

The plugin language is the plugin author's choice: Node, Python, a shell script, Rust. That
is the entire point of paying for a process boundary, and it is why the transport is
newline-delimited JSON rather than something more efficient. The measured cost of a stdio
JSON-RPC round trip on this machine is 80.6 µs for an empty result, 118.3 µs for 4 KB and
343.7 µs for 64 KB (2,000 round trips, Node parent to Node child,
`docs/09-platform-tier.md` §2.3). Against one model round trip of about 1,200 ms, a plugin
call is 0.007% of a turn: a thousand of them cost 80 ms. Speed is therefore not a reason to
make plugins harder to write — but it is a reason not to put a plugin in a hot loop of
thousands of tiny calls (§2.3 says the same thing: batch, or write that one in Rust).

## 1. Manifest

A plugin is a directory containing `plugin.json`:

```json
{
  "name": "example",
  "version": "1.0.0",
  "description": "what it does, one line",
  "runtime": "node",
  "entry": "index.js"
}
```

| Field | Required | Rule |
| --- | --- | --- |
| `name` | yes | `^[a-z0-9][a-z0-9._-]{0,63}$` — a slug, because it becomes a tool prefix |
| `version` | yes | free text; recorded and reported, not compared |
| `description` | yes | one line; it is what the operator reads when choosing |
| `runtime` | yes | exactly `node`, `python`, or `exec` |
| `entry` | yes | a path relative to the plugin directory that must not leave it |

The command line is derived, never read from the manifest:

| `runtime` | argv |
| --- | --- |
| `node` | `node <plugin-dir>/<entry>` |
| `python` | `python <plugin-dir>/<entry>` |
| `exec` | `<plugin-dir>/<entry>`, run directly (must be executable) |

A manifest that could name an arbitrary command line is a manifest that can be edited to
run one. So `runtime` is a closed set and the command is the host's business.

A manifest is refused at parse time with one of three codes, deliberately distinct because
each sends a different person somewhere different:

| Refusal | Code | What happened |
| --- | --- | --- |
| `name` violates the pattern, or `version`/`description` is empty | `bad_manifest` | the author mistyped the plugin's identity |
| `entry` is absolute or climbs out (`../`) | `bad_manifest` | the plugin would escape its own directory |
| `runtime` is not `node`/`python`/`exec` | `unsupported_runtime` | the operator installed something this host cannot start; the message lists the accepted values |
| `entry` is absent or empty | `spawn_failed` | the document is legible but names nothing to run |

`plugin.json` that is missing or is not valid JSON is `bad_manifest`, with the file named.

The child runs with the plugin directory as its working directory and inherits the host's
environment, so it can find `node`, `python`, and its own files.

## 2. Lifecycle

1. `Host::start(root)` and discovery read manifests only. **Nothing is spawned** until a
   plugin is explicitly loaded — listing a directory is not consent to execute what is in it.
2. `Host::load(name)` spawns the child and sends `initialize`.
3. On a successful handshake the host sends an `initialized` **notification** (no reply).
4. The host calls any method the plugin claimed in `capabilities`, one call at a time.
5. `Host::unload(name)` sends a `shutdown` **request**, waits up to 500 ms for its reply,
   closes stdin, waits up to 2 s for the process to exit, then kills it. It reports whether
   the exit was clean.

A plugin that exits at any point after a successful `initialize` is discovered on the next
call and reported as `child_exited`, with the last stderr line quoted. There is no health
monitor and no watcher thread: a plugin nobody is calling needs no watching, and a thread
whose job is noticing failures is a thread that can fail.

`unload` on a plugin that is not loaded returns "nothing to do" rather than an error, and a
second `unload` is a no-op: a caller that unloads in a destructor and again explicitly is
behaving reasonably.

## 3. Methods

Every request has `jsonrpc`, `id`, `method` and optionally `params`. Every reply has
`jsonrpc`, the **same** `id`, and exactly one of `result` or `error`.

The id rule is exact: a reply whose id differs from the call in flight is `protocol_error`,
not "close enough". A plugin that multiplexes replies and got them mixed up would otherwise
have one call's result silently delivered to another.

### `initialize`

Params:

```json
{ "protocolVersion": 1, "client": { "name": "hearth", "version": "0.1.0" } }
```

Result:

```json
{
  "protocolVersion": 1,
  "name": "example",
  "version": "1.0.0",
  "capabilities": { "tools": true, "prompts": true, "hooks": ["before_turn", "after_tool"] }
}
```

`protocolVersion` must be `1`. Answering another version — or a JSON-RPC error such as
`-32000` — makes the host refuse the plugin with `protocol_error` and kill the child. Do not
"mostly" agree: a v1 host and a v2 plugin that nearly match will disagree about something
that matters, and the disagreement will be discovered by a user.

`capabilities` is how the host knows **what not to ask**:

| Field | Default | Meaning |
| --- | --- | --- |
| `tools` | `false` | the plugin answers `tools/list` and `tools/call` |
| `prompts` | `false` | the plugin answers `prompts/list` and `prompts/get` |
| `hooks` | `[]` | the phases this plugin runs; only `before_turn` and `after_tool` exist |

A plugin that omits `prompts` is never asked for prompts. Calling a method the plugin did
not claim is a host bug, not a plugin bug.

### `initialized`

A notification from host to plugin: no `id`, no reply, params absent. It confirms the
handshake. A plugin may ignore it.

### `tools/list`

Params: none. Result:

```json
{ "tools": [ { "name": "echo", "description": "says it back", "schema": { "type": "object" } } ] }
```

`schema` is a JSON Schema object describing the call's `arguments`; an absent schema is read
as `{}`, never `null`. Tool names are namespaced by the host as `<plugin>.<tool>`, so a
plugin cannot shadow a builtin tool.

### `tools/call`

Params: `{ "name": "echo", "arguments": { "text": "hi" } }` (`arguments` defaults to `{}`).

Result: `{ "output": "text the model reads", "isError": false }`

`output` is a string, and it is passed through verbatim — including leading and trailing
whitespace, because the host does not edit what the model reads.

`isError: true` means the call **ran and failed**: `output` is the message the model should
see. This is not a protocol error. The distinction matters because a tool-level failure
usually teaches the model something ("that path does not exist"), while a broken transport
teaches the operator. A tool that fails must therefore answer with `isError`, not with a
JSON-RPC error.

A JSON-RPC error from `tools/call` means "I could not attempt this at all" — unknown tool
name (`-32602`), bad arguments (`-32602`), or a bug inside the plugin (`-32603`).

### `prompts/list` and `prompts/get`

`prompts/list` params: none. Result:

```json
{ "prompts": [ { "name": "greeting", "description": "one line for the model" } ] }
```

`prompts/get` params: `{ "name": "greeting" }`. Result: `{ "text": "a system-prompt fragment" }`

Prompts are content, not code. The host concatenates them in the order the operator
configured and never interprets them.

### `hooks/run`

Params: `{ "phase": "before_turn" | "after_tool", "payload": { ... } }`

Result: `{ "payload": { ... } }` — the possibly-modified payload, which replaces the host's
copy. The payload is always a JSON object and the answer must be one too; a hook that
returns a string or an array is `bad_response`, because a turn's context cannot become a
string.

A hook that wants to leave the payload alone returns it unchanged. A hook that wants to add
a field returns the whole object with the field added — the host does **not** merge; the
returned object is the new payload. A hook that fails is reported and the payload is left
untouched: a broken hook must not be able to change a turn.

The phases are a closed set (`before_turn`, `after_tool`). A plugin that answers a phase it
did not claim is a plugin the host never asks.

`after_tool` is also how a **tool decorator** is expressed: the payload carries the tool
name, its arguments and its result, and the hook returns a payload whose result it may
rewrite — redact a field, append a hint, replace an error with a shorter one. There is no
separate `decorate` method because a decorator is exactly "a hook that runs after a tool and
returns a changed payload"; a second method would be the same call with a second set of
rules to keep in sync.

### `shutdown`

A request from host to plugin: params absent, result `{}`. **Answer it, then exit with
status 0.** The host waits 500 ms for the reply, then closes stdin; a plugin that reads
until EOF also exits on its own. Exiting on `shutdown` is what makes `unload` report a clean
exit; a plugin that ignores it is killed after two seconds.

Node note, learned the hard way: `process.exit(0)` immediately after `process.stdout.write`
can truncate the reply on a pipe. Exit from the write callback instead:

```js
process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} }) + '\n',
  function () { process.exit(0); });
```

## 4. Errors

A JSON-RPC error object is `{ "code": <int>, "message": "<text>", "data": <any> }`.

| Code | Meaning |
| --- | --- |
| `-32700` | parse error: the line was not JSON (the id is `null`) |
| `-32600` | invalid request: not a JSON-RPC request |
| `-32601` | method not found |
| `-32602` | invalid params |
| `-32603` | internal error inside the plugin |
| `-32000` | the plugin cannot honour this request (e.g. an unsupported protocol version) |

A plugin may use any number in a reply; the host reports the number and the message, it does
not interpret them. The host never maps a plugin error onto a panic and never retries: a
retry of a call the plugin already executed is how one tool call becomes two side effects.

### The host's own failure codes

These are what `Error::code()` returns and what the CLI prints, one per fix:

| Code | Who has to act |
| --- | --- |
| `unknown_plugin` | operator: no directory of that name under the plugin root |
| `bad_manifest` | author: `plugin.json` is missing, unparseable, or breaks a stated rule |
| `unsupported_runtime` | author/operator: `runtime` is not one of the three |
| `spawn_failed` | operator: the manifest named nothing to run, or the interpreter is missing |
| `not_initialized` | caller: `load` was never called for this plugin |
| `protocol_error` | plugin: wrong id, wrong version, neither result nor error, unknown hook phase |
| `timeout` | plugin: no answer within the deadline; the child was killed |
| `child_exited` | plugin: it died, or closed its pipes |
| `too_large` | plugin: a reply exceeded the byte cap; the line was dropped, not buffered |
| `bad_response` | plugin: it answered, and the answer was not what the method returns |

`bad_response` quotes the first 200 bytes of a malformed reply — enough to recognise a stray
`console.log` or a stack trace, short enough not to become a five-megabyte error message.

### Plugin-initiated messages

A plugin may send notifications (`{"jsonrpc":"2.0","method":"..."}` with no id) at any time;
the host logs and skips them. A plugin-initiated **request** is answered with `-32601` and a
message saying this host takes no callbacks, rather than being dropped: a plugin waiting for
a reply it will never get would hang until its own deadline. There is no callback surface by
design — see §6.

## 5. Limits

| Limit | Default | Behaviour when exceeded |
| --- | --- | --- |
| reply bytes | 4 MiB | `too_large`; the over-long line is counted and discarded, never buffered |
| request bytes | 4 MiB (same cap) | `too_large`; the host refuses to write the frame |
| call deadline | 10 s | `timeout`; the child is killed and its handle closed |
| stderr line | 4 KiB | truncated with `[truncated]` before logging |
| stderr tail | 200 lines | oldest dropped; only the tail is kept for diagnostics |
| `close` patience | 500 ms + 2 s | `shutdown` reply wait, then stdin close and exit wait, then kill |

The reply cap is enforced **while reading**, not after: a plugin that writes a 5 MiB line has
already cost the host 5 MiB of allocation if the line is buffered first, which is the exact
failure the cap exists to prevent. Bytes past the cap are counted and dropped, and the line
is drained to the newline so the stream stays on a frame boundary and the plugin remains
usable afterwards.

Both `timeout` and `too_large` name the plugin and the method: "a plugin timed out" without
a name sends the operator to read every plugin they have.

## 6. What this protocol deliberately does not have

- **No streaming.** A tool call returns one result. Streaming needs a cancellation design
  that no plugin needs yet; a plugin with a long job should write a file and return the path.
- **No host callbacks.** A plugin cannot ask the host to run a tool, read a file, or start a
  turn. That direction would make the plugin a second agent, and the point of a plugin is to
  be smaller than the host.
- **No capability negotiation beyond `capabilities`.** No version ranges, no optional method
  sets. A plugin speaks v1 or is refused.
- **No transport other than stdio.** No sockets, no HTTP, no shared memory. A plugin that
  wants to be a server can start one; the host does not need to know.
- **No automatic restart.** A plugin that dies stays dead until `load` is called again.
  Restarting behind the caller's back would replay side effects it cannot see.
