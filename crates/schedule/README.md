---
dir: crates/schedule
layer: L2
role: recurrence rules and the scheduled-task registry
public: [Rule, Freq, Weekday, Task, Due, Registry, parse_rule]
may_import: [forge-core]
must_not: [any I/O beyond the registry file, any model or session knowledge]
gate: cargo test -p forge-schedule --offline
---

# forge-schedule

## 做什么

`forge-schedule` 拥有一件事：**一个计划任务下一次何时发生**。它解析 RFC 5545
`RRULE` 的一个子集，把规则变成一串 UTC 毫秒时刻，并把「规则 + prompt」这一对
持久化到 `<root>/tasks.json`，与任何会话日志无关——没有会话时任务也还在，重启后
任务也还在。

这一层是整个时间能力的**判据**：`next_after` 的期望值是一张手算的 UTC 时刻表，
测试拿它当 oracle。因此这里的每个函数都是纯函数：凡是需要「现在」的地方都由调用方
传入 `now_ms`，库内没有任何时钟读取。同一份规则、同一个 DTSTART、同一个锚点，
在任意机器、任意时区、任意一天都给出同一个整数。

时间模型只有一种：`i64` Unix 纪元毫秒，UTC，无闰秒、无夏令时（UTC 没有夏令时，
跳过 DST 就是把一个坑换成另一个坑）。

## 公开面

- `Freq`：`Secondly` / `Minutely` / `Hourly` / `Daily` / `Weekly` / `Monthly` /
  `Yearly`，即 `interval` 的步长单位。
- `Weekday`：`0 = 周一 … 6 = 周日`，常量 `MO TU WE TH FR SA SU`；`index()` 取值，
  `as_str()` 取两字母名。
- `Rule { freq, interval, by_day, by_hour, by_minute, count, until_ms }`：
  `Rule::parse(&str) -> Result<Rule>`；`next_after(start_ms, after_ms) -> Option<i64>`
  返回**严格大于** `after_ms` 的第一个发生时刻，且永不早于 `start_ms`；
  `occurrences(start_ms, after_ms, limit) -> Vec<i64>` 按序返回至多 `limit` 个。
  `parse_rule(&str)` 是同一件事的自由函数形式。
- `Task { id, rule, prompt, target, created_ms, start_ms, last_run_ms, runs }`：
  `rule` 原样保存规则文本（未来版本可以重新解析），`Task::next_after_ms()` 用
  `last_run_ms`（没有则 `start_ms`）作锚点重新解析规则。
- `Due { task, at_ms }`：一条到期的任务和它到期的那一个时刻。
- `Registry`：`open(root)`、`path()`、`load()`、`save(&[Task])`、`add(Task)`、
  `remove(id)`、`get(id)`、`due(now_ms)`、`mark_run(id, at_ms)`。文件缺失等于空表
  （不是错误）；写入先写同目录临时文件再 `rename`，所以读到的永远是完整 JSON。

解析接受：`FREQ`（必需）、`INTERVAL`、`BYDAY`、`BYHOUR`、`BYMINUTE`、`COUNT`、
`UNTIL`。除此之外的部件都报 `Invalid` 并点名该部件；带序号前缀的 `BYDAY`
（`1MO`、`-1FR`）明确拒绝——本子集没有「第几个周几」。`COUNT` 与 `UNTIL` 同时出现
也拒绝（RFC 5545 §3.3.10 不允许；静默偏向一个会让两个客户端算出两套时间）。

子集里几处必须写死的语义（不是 RFC 的默认值，是这一层的决定）：

- `start_ms` 只做三件事：给出默认的时/分/秒、给出 `WEEKLY` 的默认星期、给
  `interval` 定相位。**它本身不会被投递**：`next_after` 严格大于锚点，而 `Task`
  的锚点是 `last_run_ms` 或 `start_ms`。
- `BYHOUR` / `BYMINUTE` 是**展开**：每天/每周/每月/每年选中的那一天，按
  `by_hour × by_minute` 的笛卡尔积各发生一次；秒和毫秒沿用 DTSTART。
- `BYDAY` 在 `WEEKLY` 里是**展开**（一周内选中的每一天），在 `DAILY` 里等价于过滤，
  在 `MONTHLY` 里是**展开**（当月每个匹配的周几，因为子集没有 `BYMONTHDAY`），在
  `YEARLY` 里是**过滤**（候选日始终是 DTSTART 的月-日；把一整年的周几都展开成 52 次
  不是任何人想要的，而且那样就构造不出「永不发生」的规则）。
- `MONTHLY` 的日号超过当月长度时**夹到月末**（31 号在 2 月落在 29/28 号），
  这样「每月 31 号」不会一年跳过五个月。
- `YEARLY` 的 2 月 29 日在平年**不夹、直接跳过**，下一次是下一个闰年。
  这两条有意不同，测试各自钉住一个例子。
- 只有日期没有时间的 `UNTIL=YYYYMMDD` 表示「到那一天的最后一毫秒」，否则截止点会
  取决于 DTSTART 的小时。
- 搜索有界：`SECONDLY/MINUTELY/HOURLY` 至多 50 万步，`DAILY` 至多 3700 个周期，
  `WEEKLY` 1000 个，`MONTHLY` 1200 个，`YEARLY` 500 个；`occurrences` 一次至多
  取 10000 个。命中不了就返回 `None`/空表，而不是无限循环。

## 依赖与边界

只依赖 `forge-core`（错误种类、`new_id()`）和标准库；`serde` / `serde_json` 是
工作区已有的依赖，用来读写那一份注册表文件。不引入 rrule crate、不引入日期库：
日历换算（Howard Hinnant 的 era 算法）在本 crate 内，三十行，可以被手算值逐条验证。

I/O 只有一处：`Registry` 打开的那个 `tasks.json`（以及同目录的临时文件）。不遍历
目录、不读配置、不碰会话日志、不知道模型和工具的存在。`Registry` 不缓存、不做
文件锁：调度器是单进程的，重复读的成本远小于让两份内存状态分叉的成本。

## 不要做

- 不要补全 RFC 5545：`BYSETPOS`、`BYMONTH`、`BYMONTHDAY`、`BYYEARDAY`、
  `BYWEEKNO`、`WKST`、带序号的 `BYDAY` 都不在子集里。要做「每月最后一个周五」，
  那是另一个决定，先写进契约。
- 不要在库代码里读时钟。需要「现在」的调用方传 `now_ms`；这是 oracle 可重复的
  唯一原因。
- 不要放宽 `COUNT` 与 `UNTIL` 的互斥，也不要在两者同时出现时挑一个。
- 不要把 `dtstart` 当成第一次投递：锚点是「上一次跑到哪」，第一次发生在锚点之后。
- 不要 `unwrap()`/`expect()` 解析结果与 I/O 结果；坏规则是 `Invalid`，文件坏了是
  `Json`，任务不存在是 `NotFound`，id 重复是 `Conflict`。
- 不要在 `save` 里重写整个目录或扫描文件：只写 `tasks.json` 和它的临时文件。
- 不要把读不出来的规则当成致命错误：`due` 跳过它（未来版本写的规则文本留在文件里，
  这个版本不认识就不投递），而不是让整张表报错。