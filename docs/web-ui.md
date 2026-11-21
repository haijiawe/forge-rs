# 负载下的 Web 转录

这篇要了结的主张：**一个会话里 5,000 条消息，仍然能跑在 60 fps。** 它是移植清单上最后一条没有实测的项，因为测它需要真浏览器，而先前的单元测试分不清"虚拟化列表"和"坏掉的列表"。

## 方法

一切都跑在发布出去的二进制和发布出去的 bundle 上——没有测试 harness，没有 mock：

1. `hearth` 用产品自己的信封生成了一个会话日志：2,500 个 turn、5,000 条消息、10,001 个事件、1,997,455 字节，手写生成，所以字节形状跟真日志一致（规范键序、`seq` 密集）。
2. `bin/hearth.exe web --no-browser --addr 127.0.0.1:8517` 把它提供出去。
3. Microsoft Edge（`--headless=new`、`--window-size=1400,900`、`ANGLE (NVIDIA GeForce RTX 4060 Direct3D11)`）通过 CDP 打开真实 UI。
4. 用 `requestAnimationFrame` 打点：空闲 3 秒，然后以每帧 120 px 滚动转录，再 3 秒。

## 结果

连续三轮，同机、同会话：

| 轮次 | 加载 | DOM 里的行数 | 空闲 fps | 滚动 fps | 最长任务 | 堆 | 节点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 686 ms | 27 | 54.5 | 60.0 | 75 ms | 18.7 MB | 208 |
| 2 | 649 ms | 27 | 56.5 | 60.0 | 55 ms | 17.2 MB | 208 |
| 3 | 610 ms | 27 | 58.3 | 60.0 | 0 ms | 17.9 MB | 208 |

滚动那个数字才是这条主张针对的东西，而它是稳定的 60 fps：转录通过挂载 27 行、208 个 DOM 节点，滚动一份 **480,130 px** 的文档。空闲时比 60 低几帧，是因为 headless Chromium 在没有动画时会节流自己的合成器；滚动时不会。

## 浏览器找到的、测试套件找不到的东西

三个缺陷对仓库里的每个测试都不可见，而每一个都让真实浏览器里的 UI 实时流失效：

1. **cookie 从来没被设过。** `api.md` §2 说浏览器出示 `HttpOnly` 的 `hearth_token` cookie，并且"操作者带外设置它"——但 `hearth web` 打印的 URL 带 `?token=…`，两者之间没有任何桥接，于是流在每次重试时都答 `401`。现在当 `token` 查询参数匹配时，shell 会回 `Set-Cookie: hearth_token=…; HttpOnly; SameSite=Strict; Path=/`（token 不对时仍然返回 shell，只是不带 cookie）。
2. **握手带不了版本。** `api.md` §3 说 WebSocket 客户端在升级时发 `X-Hearth-Protocol`——一个浏览器设不了的请求头。现在客户端可以改为提议 `hearth.v1` 子协议，服务端在 `101` 里回显它（RFC 6455 §4.2.2）。两个信号都没有时仍然是 `400`。
3. **`Sec-WebSocket-Accept` 没有补位。** 服务端的 `base64_encode` 从不补位，因为其他调用方都要无补位的 base64url（token、游标）。SHA-1 是 20 字节，所以正确的 accept 总以一个 `=` 结尾——而浏览器是逐字节比较的。仓库自己的测试把这一点藏了两次：对 RFC 规范样例 key 的断言*去掉了*它的 `=`，WebSocket 语料在比较前把两边的 `=` 都裁掉了。现在两处断言都要求补位，并且 `a_browser_style_upgrade_authenticates_by_cookie_and_names_its_subprotocol` 钉住了一个 accept 必须补位的 key。

## 这篇没有测什么

- 只有本机 Windows/amd64 上的 headless Edge。没有其他浏览器，没有软件光栅化，没有低端 GPU。
- 空闲 fps 按实测报（54–58）；那是合成器节流，不是丢工作，但它不是 60 fps 的主张。
- 滚动是程序驱动的。人手一甩、触控板、惯性滚动都没有测。
- 对很大的积压，转录停在 `scrollTop: 0` 打开，并给一个 `↓ latest` 按钮——与已退役的 Go 构建走同一条代码路径（`forge-go/web/src/components/Transcript.tsx`，2026-11-17 删除），所以这是产品的设计，不是移植回归。验证方式是分别加载一个 81 事件会话和一个 10,001 事件会话：两者都在顶部打开。