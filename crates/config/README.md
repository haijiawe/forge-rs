---
dir: crates/config
layer: L0
role: config file, home directory, defaults, and MCP entry validation warnings
public: [Config, SandboxAccessConfig, MCPServerConfig, MCPConfig, MCPConfigError, SkillsConfig, SubagentConfig, PluginsConfig, ExpertsConfig, TeamConfig, load, home_dir, default_path, plugin_root, expert_roots, SANDBOX_READ_ONLY, SANDBOX_WORKSPACE_WRITE, SANDBOX_FULL_ACCESS]
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
  `mcp_warnings()`, `usable_mcp_servers()`, `skill_roots()`, `sandbox_allow_net()`；字段
  `profile`, `model`, `data_dir`, `sandbox`, `sandbox_access`, `debug`, `mcp`, `skills`,
  `subagent`。
- `MCPServerConfig` / `MCPConfig` / `SkillsConfig` / `SubagentConfig`：与 Go 配置相同的
  JSON 形状（`servers`, `roots`, `maxDepth`, `timeoutMs` 等）。
- `SandboxAccessConfig`：`sandboxAccess` 对象，三个成员 `read`、`write`、`network`。
  `read`/`write` 是对沙箱默认授权的整体替换（显式空列表表示“什么都不额外授权”）；
  `network` 是布尔值，决定沙箱子进程能不能用网络，缺省即 `false`。
  三个成员都是 `Option`，因为“没写”和“写了 false/空列表”必须可区分：
  前者落到默认值，后者是操作者的明确决定。`sandbox_allow_net()` 是读 `network` 的
  唯一入口——它把“缺省”和“显式 false”收敛成同一个有效答案 `false`。
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
- 不要把 `sandboxAccess.network` 写成裸 `bool` + `#[serde(default)]`：那样“没写”和
  “写了 false”会合并成同一个值，以后想改默认值就没有据可依了。
