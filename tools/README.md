# tools/ — 两个不产出产品功能的 crate，只负责「不许退步」

它们不是产品的一部分：`hearth` 运行时不依赖它们。它们存在的理由是**把口头规矩变成退出码**。
一条只写在文档里的规则，会在第一次赶时间的时候消失；一条会红的检查不会。

## 里面有什么

| 目录 | 是什么 | 什么时候跑 | 说明书 |
| --- | --- | --- | --- |
| [`archcheck/`](archcheck/README.md) | 依赖方向门禁：分层、README、文档指针 | 每次 `cargo test` 之前，CI 也在跑 | [archcheck/README.md](archcheck/README.md) |
| [`bench/`](bench/README.md) | 六项指标的基准与基线比对 | 想证明「没变慢」的时候，`-strict` 才判失败 | [bench/README.md](bench/README.md) |

## archcheck 会拦住什么

十一种发现，每一种都有稳定的机器可读代码，`-json` 输出给 CI 用：

| 代码 | 意思 |
| --- | --- |
| `edge.upward` | 某个 crate 依赖了更高层 |
| `edge.forbidden` | 出现了 `architecture.yaml` 明确禁止的边（例如 `core → config`） |
| `edge.unknown` | 依赖了一个不在分层图里的名字 |
| `crate.undeclared` | `Cargo.toml` 用了某个 crate 但没在分层图里声明 |
| `crate.duplicate` | 一个 crate 被放进两层 |
| `crate.missing` | 分层图声明了一个不存在的 crate |
| `readme.missing` / `readme.unreadable` | crate 没有说明书，或读不出来 |
| `readme.frontmatter` / `readme.key` / `readme.layer` | 七键缺失、写错、layer 与分层图不一致 |
| `readme.section` | 缺 `做什么` / `公开面` / `依赖与边界` / `不要做` 里的一节 |
| `docs.missing` | `hearth help` 里提到的某个 `docs/*.md` 不存在 |

退出码：`0` 干净、`1` 有发现、`2` 用法错误。`-root <路径>` 可以检查另一棵树。

## 新东西放哪

- **一条能自动判断的规矩** → `archcheck/src/` 里加一个检查函数，给它一个 `code`，
  并在 `tools/archcheck/README.md` 的表里加一行。加规则要顺手加一个**会红的测试**。
- **一个要量化的性能承诺** → `bench/src/` 一个指标 + `scripts/bench/baseline.json` 记录基线与测量命令。
  没有基线的数字不是承诺，是印象。

## 相关

- [architecture.yaml](../architecture.yaml) — archcheck 读的那张分层图
- [docs/bench.md](../docs/bench.md) — 每个数字是怎么测出来的
- [docs/ci.md](../docs/ci.md) — 这些门禁在 runner 上抓到过什么