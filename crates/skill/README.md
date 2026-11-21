---
dir: crates/skill
layer: L1
role: skill discovery and on-demand loading of SKILL.md instruction files
public:
  - Skill
  - Set
  - scan
  - Scan
  - SkillError
  - SkillErrorKind
may_import: []
must_not:
  - depend on any hearth crate or any external crate
  - recurse into skill directories or write any file
  - guess at YAML structure that is not a scalar key: value pair
gate: cargo test -p hearth-skill --offline
---

## 做什么

在 `root/*/SKILL.md` 发现技能——也就是每个给定根目录下的一层——并且只在被要求时才加载它们的正文。根目录缺失是警告而不是失败：扫描带着剩下的根继续，这样一条坏路径不能把其余的都藏起来。

所有没能产出技能的东西都是数据：`Set::skipped` 与 `Set::warnings` 携带排好序的、给人读的原因。`Scan` 从不因为文件系统问题返回错误；`Skill::load` 会，因为一个要某个具体正文的调用方需要知道那次失败了。

## 公开面

`Skill { name, description, dir, path, body, files }` 是 `Clone + Debug + PartialEq`，所以渲染出来的 `Set` 可以和金样逐字节比对。`Set { skills, skipped, warnings }` 有相同的 derive。

`scan(&[PathBuf]) -> Result<Set>` 是符合习惯的入口；`Scan` 是同一个函数，用的是冻结契约里那套与 Go 兼容的拼写。`Skill::load(&self) -> Result<Skill>` 返回一份填好 `body` 的副本，从不修改接收者。错误用 `SkillError` / `SkillErrorKind`。

## 依赖与边界

只用标准库。没有 hearth 内部依赖，没有外部 crate，没有 async 运行时。所有文件系统访问都走 `std::fs` 与 `std::path`；只有**输出**字符串被规范化成正斜杠，好让金样在 Windows 与 Unix 上保持一致。

边界是「发现并读取，绝不做决定」：本 crate 报告它找到了什么，不判断某一轮该用哪个技能。策略与调用属于 L1 之上。

## 不要做

- 不要递归进技能目录。`Files` 只列同级的普通文件，排除符号链接目录，按相对路径排序。
- 不要静默丢掉一个结构化的 frontmatter 值。嵌套值或块标量会产生一条警告并把该键移除；不做任何猜测。
- 不要整份读一个无界文件。读取上限是 1 MiB，上限处不完整的 UTF-8 rune 会被裁掉，而且只有 `Skill::load` 会读正文。
- 不要让诊断顺序取决于目录遍历顺序。技能、跳过项与警告在返回 `Set` 之前都已排好序。