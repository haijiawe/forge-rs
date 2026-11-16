---
dir: crates/cli
layer: L3
role: the forge binary: argument parsing, usage, version, and doctor
public: [run, reorder_flags, VERSION, COMMIT, USAGE]
may_import: [forge-core, forge-config, forge-sandbox]
must_not: []
gate: make arch
---

# forge-cli

## 做什么

`forge-cli` 提供 `forge` 可执行文件。它把参数解析、帮助文本、版本快路径和 `doctor` 报告集中在
一个可测试的 `run(args, stdout, stderr)` 入口里，`main.rs` 只负责取进程参数和设置退出码。
`--version` 不读取配置：配置文件损坏时版本仍必须可用。`doctor` 按固定顺序执行十项只读检查，
一切无法执行的检查都报告 `SKIP`，从不伪装成 `OK`。

## 公开面

- `run(&[String], &mut dyn Write, &mut dyn Write) -> i32`：完整 CLI 分派；返回 0/1/2 退出码。
- `reorder_flags(&[String], &[&str]) -> Vec<String>`：把 flag 移到位置参数之前，供后续
  `chat`/`import` 共用；`--` 之后的词保持位置参数顺序。
- `VERSION`, `COMMIT`, `USAGE`：版本串与冻结契约文本。
- `doctor` 子模块：`Report`/`Check` 的文本表格和稳定 JSON（`rustVersion` 键）。

## 依赖与边界

可依赖所有更低层 crate：当前是 `forge-core`、`forge-config`、`forge-sandbox`。不得被其他 crate
导入（`main.rs` 是唯一入口），也不得把 clap 之类的解析器或 async 运行时带进来——参数解析是手写的，
与 Go 版保持一致。

## 不要做

- 不要在 `--version` 路径上读取配置；快路径必须保持快且可用。
- 不要在 `doctor` 里写系统状态；唯一允许的写是数据目录里的临时探针，且必须删除。
- 不要增删 `help` 的九行契约文本：`USAGE` 必须与 `docs/contracts/cli.md` §2.2 逐字节一致。
  本波只实现 `version`/`help`/`doctor`，其余契约命令名仍未实现，这一已知偏差在交付报告的
  `open` 一节披露；不要报告未执行的检查为 `OK`。
- 不要在库代码里对 I/O 或解析结果使用 `unwrap()`/`expect()`。
