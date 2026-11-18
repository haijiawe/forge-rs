---
dir: crates/config
layer: L0
role: config file, home directory, defaults, and MCP entry validation warnings
public: [Config, MCPServerConfig, MCPConfig, MCPConfigError, SkillsConfig, SubagentConfig, PluginsConfig, ExpertsConfig, TeamConfig, load, home_dir, default_path, plugin_root, expert_roots, SANDBOX_READ_ONLY, SANDBOX_WORKSPACE_WRITE, SANDBOX_FULL_ACCESS]
may_import: [forge-core]
must_not: [forge-exec, forge-sandbox, forge-session, forge-llm, forge-tool, forge-protocol, forge-server, forge-cli]
gate: make arch
---

# forge-config

## 做什么

`forge-config` 负责 `~/.forge/config.json`（或 `FORGE_HOME` 指向的目录）的读取、默认值与校验。
配置里的未知成员被忽略，这样更新的配置可以交给旧版本加载；缺失文件不是错误，直接返回默认值。
MCP 条目的错误不会让整份配置加载失败：坏条目被跳过并记录到 `warnings()`，其余服务器照常可用。
这个选择是刻意的——一个服务器的拼写错误不能阻止整个运行时启动。

## 公开面

- `Config`：`defaults()`, `load(&Path)`, `validate()`, `ensure_data_dir()`, `warnings()`,
  `mcp_warnings()`, `usable_mcp_servers()`, `skill_roots()`；字段 `profile`, `model`,
  `data_dir`, `sandbox`, `debug`, `mcp`, `skills`, `subagent`。
- `MCPServerConfig` / `MCPConfig` / `SkillsConfig` / `SubagentConfig`：与 Go 配置相同的
  JSON 形状（`servers`, `roots`, `maxDepth`, `timeoutMs` 等）。
- `home_dir()` 与 `default_path()`：先看 `FORGE_HOME`，否则用户主目录下的 `.forge`。
- 沙箱模式常量 `SANDBOX_READ_ONLY`、`SANDBOX_WORKSPACE_WRITE`、`SANDBOX_FULL_ACCESS`。

## 依赖与边界

只依赖 `forge-core` 与标准库（外加 serde/serde_json），不得导入任何更高层的 crate。
配置层不知道沙箱如何安装、会话如何写日志，也不执行任何命令；`ensure_data_dir()` 只创建数据目录
并写入再删除一个探针文件，不修改配置本身。

## 不要做

- 不要把未知的 JSON 成员当作错误；旧版本必须能读新配置。
- 不要让一个坏 MCP 条目阻止整份配置加载。
- 不要在库代码里对 I/O 或解析结果使用 `unwrap()`/`expect()`。
- 不要把 `dataDir` 为空当作合法配置（回落到默认目录）。
