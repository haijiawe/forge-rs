---
dir: crates/tool
layer: L2
role: registry, executor, policy, approvals and the builtin tools
public: []
may_import: [forge-core, forge-exec, forge-llm]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The tool system: a `Tool` trait, a registry, an executor that enforces policy and approvals before anything runs, and the seven builtin tools (fs read/write/edit/glob/grep, shell, todo). Ported in W3.

## 公开面

Nothing yet — placeholder until W3. Planned: `Tool`, `Call`, `Result`, `Registry`, `Executor`, `Policy`/`Rule`/`Decision`, `Approver`.

## 依赖与边界

Do not let the executor decide policy. Policy is data (`Rule`s) merged by the caller; an executor that hard-codes "ask for shell" cannot be tested without a shell.

## 不要做

- Do not write code here before the wave that owns it. A placeholder that
  compiles is honest; a half-implementation that looks finished is not.
- Do not add a dependency outside the workspace budget in Cargo.toml.