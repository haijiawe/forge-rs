---
dir: crates/tool
layer: L2
role: registry, executor, policy, approvals and the builtin tools
public: [Tool, Call, Result, Limits, DefaultLimits, Registry, Executor, Policy, Rule, Rules, Decision, Approver, ApprovalRequest, AllowAll, Dangerous, builtin]
may_import: [forge-core, forge-exec, forge-llm]
must_not:
  - import a crate from a higher layer
  - write event-log or approval events; session owns the log
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The tool system: a `Tool` trait, a registry, an executor that enforces policy and approvals before anything runs, and the seven builtin tools (`fs_read`, `fs_write`, `fs_edit`, `fs_glob`, `fs_grep`, `shell`, `todo_write`). The executor never writes events; the loop records the call and result from the `Call` and `Result` values.

## 公开面

`Tool`, `Call`, `Result`, `Limits`, `DefaultLimits`, `Registry`, `Executor`, `ExecutorOptions`, `Policy`, `PolicyFunc`, `Rule`, `Rules`, `Decision`, `Approver`, `ApprovalRequest`, `AllowAll`, path helpers, truncation helpers, `Dangerous`, and the `builtin` module with `all_tools()`.

## 依赖与边界

Do not let the executor decide policy. Policy is data (`Rule`s) merged by the caller; an executor that hard-codes `ask` for shell cannot be tested without a shell. Shell commands use `forge-exec` only. Filesystem paths are resolved under the caller-supplied workspace and are never inferred from the process CWD.

## 不要做

- Do not import `forge-session`, `forge-agent`, `forge-server`, or any higher layer.
- Do not add a dependency outside the workspace budget in the workspace `Cargo.toml`.
- Do not run a tool before policy allowance and, when required, approver consent.
- Do not emit unbounded output; every tool result passes through `truncate`.
