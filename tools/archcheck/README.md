# archcheck — 把口头规矩变成退出码

`archcheck` 回答三个 Cargo 回答不了的问题：

1. **依赖边只能指向下层或同层。** `Cargo.toml` 能证明依赖存在，证明不了方向对。
2. **每个 crate 都有一份说明书**，写清它守的那条边界。
3. **`hearth help` 承诺的路径存在**，以及**每个容器目录都有那一页解释它的文字**。

它不是产品的一部分：`hearth` 运行时不依赖它。它存在的理由是——一条只写在文档里的规则，
会在第一次赶时间的时候消失；一条会红的检查不会。

## 怎么用

```powershell
cargo run -q -p archcheck --offline          # 检查当前树
cargo run -q -p archcheck --offline -- -json # CI 用，机器可读
cargo run -q -p archcheck --offline -- -root <路径>  # 检查另一棵树
```

退出码：`0` 干净，`1` 有发现，`2` 用法或检查器自身的错误。`-root` 缺省从工作目录往上找
`architecture.yaml`——**找不到是错误，不是空通过**：一个默默检查了错误目录的检查器比不检查更糟。

## 它会拦住什么

| 代码 | 意思 |
| --- | --- |
| `edge.upward` | 某个 crate 依赖了更高的层 |
| `edge.forbidden` | 出现了 `architecture.yaml` 明确禁止的边（如 `hearth-core → hearth-config`） |
| `edge.unknown` | 依赖了一个以 `hearth-` 开头但分层图里没有的名字 |
| `crate.undeclared` | 一个 crate 不在分层图里——它在门禁之外 |
| `crate.duplicate` | 同一个包名被两个 crate 用，或一个 crate 被放进两层 |
| `crate.missing` | 分层图声明了一个不存在的 crate（承诺了一个还没建的边界） |
| `readme.missing` | crate 没有 README |
| `readme.unreadable` | README 读不出来 |
| `readme.frontmatter` | 没有七键 frontmatter |
| `readme.key` | 七键缺一个或拼错 |
| `readme.layer` | README 里的 `layer` 与分层图不一致 |
| `readme.section` | 缺 `做什么` / `公开面` / `依赖与边界` / `不要做` 里的某一节 |
| `readme.dir` | README 里的 `dir` 与实际目录不一致 |
| `docs.missing` | `hearth help` 里提到的某个 `docs/*.md` 不存在 |
| `docs.unreadable` | CLI 源码读不出来，无法核对那条承诺 |
| `dirs.missing` | 一个被声明的容器目录没有 README |
| `dirs.empty` | README 存在但只是占位（没有标题，或不足五行内容） |
| `dirs.unknown` | `documented_dirs` 里写了一个不存在的目录 |
| `dirs.unreadable` | 那个 README 读不出来 |

## 为什么判据在 YAML 里而不是代码里

`documented_dirs` 写在 `architecture.yaml`。哪一天多了一个顶层目录，改动应该是那张表里加一行——
**那一行会被人看见**，而藏在检查器源码里的数组不会。同一份理由也适用于
`docs.rs` 不维护一份「帮助文本应该提到哪些文件」的名单：它直接读帮助文本那一行。

## 它刻意不做的事

- **不解析 Rust 源码。** 依赖关系与 README 都能从文件系统读到，读源码只会引入一个假的解析器。
- **不用 YAML crate。** 文件只有五个键，一页代码就能读完；引入依赖要回答「标准库为什么不行」，
  这里的诚实答案是「行」。
- **不扫描整棵树找没说明书的目录。** 只查被声明的那些。一条要求每个 `src/` 都配说明的规则，
  会在两天内被一批空洞的 README 喂饱。
- **不判断文档的语言与质量。** 它只能判断「有、读得出来、不是空壳」。写得对不对是评审的事。

## 相关

- [architecture.yaml](../../architecture.yaml) — 分层图与 `documented_dirs`
- [tools/README.md](../README.md) — 这个目录里还有什么
- [docs/08-rust-port.md](../../docs/08-rust-port.md) §2 — 每一层为什么这么分