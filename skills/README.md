# skills/ — 技能：给模型看的说明书，不是代码

一个技能就是**一份 Markdown 加它需要的附带文件**。模型先只见目录与描述，
真的要用时才读正文（`crates/skill` 的规矩：被要之前不读正文，否则每个会话都为全部技能付一遍上下文）。

## 里面有什么

| 目录 | 是什么 |
| --- | --- |
| [`office/`](office/SKILL.md) | 生成与检查 docx / xlsx / pptx / PDF。驱动外部程序，不自己实现 Office 格式 |

## 一个技能长什么样

```text
skills/<名字>/
  SKILL.md          # frontmatter（名字、描述、何时用）+ 正文
  其他文件           # 脚本、模板、参考数据，正文里按相对路径引用
```

`SKILL.md` 的 frontmatter 决定模型看不看得见它；正文决定模型读不读得懂。所以：

- 描述要写「**什么时候该用**」，不是「这是什么」。前者是路由，后者是目录。
- 正文写步骤。写不出步骤的技能，通常是一件该做成工具的事。

## 新东西放哪

- 一个可复用的操作流程 → `skills/<名字>/SKILL.md`，参考 `office/` 的形状。
- 需要**往返**、需要保持状态的 → 那是工具（`crates/tool/src/builtin/`），不是技能。
- 需要第三方程序 → 技能里写明依赖与缺了会怎样（`office` 就这么做：缺 LibreOffice 时 `doctor`
  打 WARN，而不是假装能转）。

## 相关

- [crates/skill/README.md](../crates/skill/README.md) — 扫描、路由与延迟加载的实现
- [docs/office-run.md](../docs/office-run.md) — `office` 技能实际跑出来的产物与限制