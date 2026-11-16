---
dir: crates/tool
layer: L2
role: registry, executor, policy, approvals and the builtin tools
public: [Tool, Call, Result, Limits, DefaultLimits, Registry, Executor, Policy, Rule, Rules, Decision, Approver, ApprovalRequest, AllowAll, Dangerous, builtin, Options, names, WebFetchTool, WebSearchTool, SearchConfig, WorktreeTool, PresentTool]
may_import: [forge-core, forge-exec, forge-llm, forge-net]
must_not:
  - import a crate from a higher layer
  - write event-log or approval events; session owns the log
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The tool system: a `Tool` trait, a registry, an executor that enforces policy and approvals before anything runs, and the builtin tools. Seven are always registered (`fs_read`, `fs_write`, `fs_edit`, `fs_glob`, `fs_grep`, `shell`, `todo_write`) and `worktree` joins them unconditionally; the platform tools appear only when their capability is configured — `web_fetch` needs a network client, `web_search` additionally needs an endpoint, `present` needs a data directory. The executor never writes events; the loop records the call and result from the `Call` and `Result` values.

## 公开面

`Tool`, `Call`, `Result`, `Limits`, `DefaultLimits`, `Registry`, `Executor`, `ExecutorOptions`, `Policy`, `PolicyFunc`, `Rule`, `Rules`, `Decision`, `Approver`, `ApprovalRequest`, `AllowAll`, path helpers, truncation helpers, `Dangerous`, and the `builtin` module with `all(&Options)`, `names(&Options)`, `Options`, the four platform tools and `SearchConfig`.

## 依赖与边界

Do not let the executor decide policy. Policy is data (`Rule`s) merged by the caller; an executor that hard-codes `ask` for shell cannot be tested without a shell. Shell and worktree commands use `forge-exec` only. Filesystem paths are resolved under the caller-supplied workspace and are never inferred from the process CWD.

Network access is `forge-net`'s job, not this crate's: a tool that reached the web by hand would be a second TLS stack with its own idea of trust roots, deadlines and redirects. A tool that needs the network takes an `Arc<forge_net::Client>` and is not registered when the caller has none, so "no network" is visible in the tool list instead of failing one call at a time.

## 不要做

- Do not import `forge-session`, `forge-agent`, `forge-server`, or any higher layer.
- Do not add a dependency outside the workspace budget in the workspace `Cargo.toml`.
- Do not run a tool before policy allowance and, when required, approver consent.
- Do not emit unbounded output; every tool result passes through `truncate`.
- Do not register a platform tool without its capability; a tool that always fails is worse than an absent one, because the model will keep trying it.