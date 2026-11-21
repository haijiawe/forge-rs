# bin/ — 手工重建的可执行文件

`bin/hearth.exe` **不是 cargo 的产物**。`cargo build` 会把它写到 `target/release/`，
而 `bin/` 里的这一份是**手动复制**过来的，原因只有一个：它内嵌了 `web/dist`，
所以「界面改了但二进制没重建」是一个真实会发生的错误，值得有一个固定位置让人一眼看到当前发布物。

## 怎么重建

三步，缺一步界面就是旧的：

```powershell
cd D:\hearth\web
npx vite build                                  # 1. 重编界面 -> web/dist

cd D:\hearth
cargo build --release -p hearth-cli --offline    # 2. include_dir! 把 dist 编进二进制

Copy-Item target\release\hearth.exe bin\hearth.exe -Force   # 3. 手工放置
```

第 2 步不能省成 `cargo build --release`：只有 `hearth-cli` 这个 crate 会内嵌界面。

## 为什么要手放

`include_dir!` 是编译期的宏。二进制**不会**在运行时去读 `web/dist`，所以：

- 只跑 `vite build` → 二进制里的界面还是旧的；
- 只跑 `cargo build`（dist 没变）→ 二进制改了但界面没改；
- 两个都跑了但忘了复制 → `bin/` 里是上一次的版本。

这三个状态看起来一模一样，都可以正常启动。因此复制是流程的一部分，不是可选的收尾。

## 相关

- [web/README.md](../web/README.md) — 内嵌的机制与 `include_dir!` 的位置
- [docs/release.md](../docs/release.md) — 发布流水线如何产出六个平台的产物与 `SHA256SUMS`
- [scripts/install.ps1](../scripts/install.ps1) — 把这里的二进制装到 `PATH` 上