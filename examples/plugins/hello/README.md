# hello — the smallest complete plugin

One tool (`greet`), no dependencies, no build step. This is the file to copy when
starting a new plugin.

## Try it

```text
forge plugin call hello greet --args '{"name":"world"}'
# hello, world
```

To make it visible to `plugin list`, put the directory under `<dataDir>/plugins/`:

```text
forge plugin new ignored-name          # only to show where the root is
forge plugin doctor                    # prints root: <dataDir>/plugins
```

Or install it from a market, which is what `forge market pack` produces:

```text
forge market pack examples/plugins/hello --out dist --base-url https://example.invalid/p
# packed hello 1.0.0 (2 file(s)) + dist/index.json
```

## Change it

1. Rename the plugin in `plugin.json` and in the `reply(...)` handshake below.
2. Change `callTool`: one branch per tool. Return `{output, isError: true}` when
   the tool ran and refused; return `null` for a name you never advertised.
3. Add a tool by appending to `TOOLS` and adding its branch.

## The three rules

These are the only ones that decide whether a plugin works; the rest is style.

| Rule | Why |
| --- | --- |
| stdout carries protocol frames only | a `console.log` is a malformed frame; diagnostics go to stderr |
| echo the request `id` exactly | the host rejects a reply whose id differs, so two calls cannot swap answers |
| `isError: true` is a result, a JSON-RPC error is a non-attempt | the model reads the first, the operator reads the second |

The full contract is [`crates/plugin/PROTOCOL.md`](../../../crates/plugin/PROTOCOL.md).