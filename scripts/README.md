# scripts/ — 装东西、发东西、量东西、查东西

五个脚本加一份基线。都不是构建的一部分：`cargo build` 不读这里，`check.ps1` 与 CI 读其中三个。

## 里面有什么

| 文件 | 干什么 | 谁用 |
| --- | --- | --- |
| [`install.ps1`](install.ps1) | Windows 安装：把 `bin/hearth.exe` 装到前缀目录并加进 `PATH` | 想在一台新机器上用它的人 |
| [`install.sh`](install.sh) | 同上，POSIX。**本机没有 bash，从未执行过**，标注在文件头 | Linux/macOS |
| [`sync.cjs`](sync.cjs) | 提交 → 走 API 推送 → 把远端新建的提交在本地重建并核对 SHA | 维护者 |
| [`check-links.cjs`](check-links.cjs) | 两条：markdown 的相对链接能解析；反引号里写的仓库路径真实存在 | `check.ps1` 与 CI |
| [`check-translations.cjs`](check-translations.cjs) | `X.zh.md` 与 `X.md` 没有走散（代码块、标题层级、表格行数、互相指名） | `check.ps1` 与 CI |
| [`bench/baseline.json`](bench/baseline.json) | 三个平台的基准数字，`hearth-bench -strict` 的比对对象 | `hearth-bench` |

## 两个检查脚本为什么存在

它们守的是同一类错误：**一句听起来没问题、但指不到东西的话**。

`check-links.cjs` 查两遍。第一遍是 markdown 的相对链接——那种错了读者点一下就知道。
第二遍是反引号里的仓库路径（形如 `crates/…/src/….rs` 的那种写法），因为散文里写错一个路径，
写的人和读的人都看不出来，只有想去找那个文件的人会浪费十分钟。它还要求已退役的旧路径
（Go 树里的、DSH 仓库里的）**在同一行**带上限定词，否则报错——旧名字本身没错，不说清楚才错。

`check-translations.cjs` 查 `docs/contracts/` 里原文与中文翻译这一对。翻译一旦和原文分叉，
它就从「帮助读者」变成「第二个真相」，而两份文件都语法正确、链接也都能解析，所以没有别的
东西会报错。它只比对结构：围栏代码块逐字节相同、标题层级序列相同、表格行数相同、
两份文件互相指名。**散文不比对**——那做不到，也不该假装做到了。

两个脚本都零依赖、只用 Node 内置模块，`node scripts/<名字>.cjs` 直接跑，不需要 `npm ci`。
`check-links.cjs` 支持传一个仓库根做参数，`check-translations.cjs` 同样。

## sync.cjs 为什么存在

这台机器上 `git push` 与 `git fetch` 直连 `github.com:443` 会挂死或连接重置，只能走 REST API。
而 API 建出来的提交对象与本地 `git commit` 建的不是同一个 SHA（它会归一化时区、吃掉消息末尾换行），
所以「推上去」之后本地分支会假性分叉。

脚本把三件事绑成一件，并且**在核对不上时拒绝移动分支**：

```powershell
node scripts/sync.cjs "提交信息"          # 提交 + 推送 + 对齐
node scripts/sync.cjs --message-file msg.txt
node scripts/sync.cjs --no-commit "..."   # 只推送已有的 HEAD
```

token 从 `$env:GITHUB_PAT` 或 `$env:HEARTH_CREDENTIALS` 指向的 YAML（默认 `D:\dsh\.credentials.yaml`）
里的 `GITHUB_PAT:` 读，从不打印。若哪天 `git push` 能用了，删掉这个脚本是**改进**而不是损失。

## 新东西放哪

- 安装方式 → 同时改 `install.ps1` 与 `install.sh`，两个文件头都写着各自的实测状态。
- 一个新的性能指标 → 先加到 `tools/bench/src/`，再 `make bench-record` 更新 `bench/baseline.json`。
  基线只记本平台跑出来的数，不要手抄。

## 相关

- [docs/release.md](../docs/release.md) — 发布流水线与 `SHA256SUMS`
- [docs/bench.md](../docs/bench.md) — 六项指标的口径
- [tools/bench/README.md](../tools/bench/README.md) — `-strict` 的判定规则