# scripts/ — 装东西、发东西、量东西

三个脚本加一份基线。都不是构建的一部分：`cargo build` 不读这里，CI 只读 `bench/`。

## 里面有什么

| 文件 | 干什么 | 谁用 |
| --- | --- | --- |
| [`install.ps1`](install.ps1) | Windows 安装：把 `bin/hearth.exe` 装到前缀目录并加进 `PATH` | 想在一台新机器上用它的人 |
| [`install.sh`](install.sh) | 同上，POSIX。**本机没有 bash，从未执行过**，标注在文件头 | Linux/macOS |
| [`sync.cjs`](sync.cjs) | 提交 → 走 API 推送 → 把远端新建的提交在本地重建并核对 SHA | 维护者 |
| [`bench/baseline.json`](bench/baseline.json) | 三个平台的基准数字，`hearth-bench -strict` 的比对对象 | `hearth-bench` |

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