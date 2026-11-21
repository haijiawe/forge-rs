---
dir: crates/market
layer: L2
role: plugin market — index fetch, resolve, checksummed download, tar/tar.gz pack and unpack, install record
public: [Index, Bundle, Engines, Platform, host_platform, KNOWN_OS, KNOWN_ARCH, INDEX_VERSION, Store, InstalledSet, InstalledRecord, InstalledFile, install, install_bytes, uninstall, list, modified_files, extract_to, Installed, Removed, download, verify_file, open_tarball, pack_dir, Packed, parse_tar, Archive, Entry, MAX_ENTRIES, unpack, Unpacked, gunzip, gunzip_limited, is_gzip, inflate, inflate_from, sha256_hex, is_sha256_hex, digest_of_file, crc32, crc16_from_crc32, is_safe_name, is_safe_version, is_safe_relative_path, is_absolute_http_url, now_rfc3339, rfc3339_from_epoch, BUNDLES_DIR, RECORD_NAME, RECORD_VERSION, STAGING_DIR, DEFAULT_MAX_OUTPUT, DEFAULT_MAX_GZIP_OUTPUT]
may_import: [hearth-core, hearth-net]
must_not:
  - import any crate above L2
  - add flate2, tar or sha2; the budget answers to "why can the standard library not do this"
  - treat a checksum mismatch as a warning
  - write outside the destination directory during an unpack
  - keep an install that failed verification
  - let pack and unpack disagree about the format; pack writes what parse_tar reads
gate: cargo test -p hearth-market --offline
---

## 做什么

一个插件市场。消费侧拉取一份 `{"version":1,"bundles":[...]}` 索引，把一个名字加一条要求解析成恰好一个 `Bundle`，用 HTTP 下载这个 bundle 的 tarball，用**实际收到的字节**验证它的 SHA-256，解包到 `<root>/bundles/<name>/` 下，并把它写了哪些文件记进 `<root>/installed.json`，这样卸载时能精确删掉这些文件。生产侧——`pack_dir`——把一个本地目录变成同一种产物。

真正要紧的是这些拒绝，不是那些下载：

- **摘要是在字节落盘之后校验的**，所以这份摘要描述的是后一次安装读到的那个文件，而不是一个被写了两次的缓冲区。不匹配会删掉文件并报出两个摘要；响应体偏短是 `truncated` 并带上两个长度；比索引声明更长的响应体直接拒绝，而不是裁到刚好。
- **进来的一路上什么都不跳过。** 只要出现本版本读不了的索引版本、格式错误的 `sha256`、为零的 `size`、不是绝对 http(s) URL 的 `tarball`，或两条条目的名字、版本**以及平台**都相同，整份索引就被拒绝。形如 `^1.2`、`1.x`、`>=1 <2` 的 semver 区间按名字拒绝而不是近似处理——半个 semver 比没有 semver 更糟。
- **什么都不许逃出目标目录。** tar 读者拒绝绝对路径、`..`、盘符与流前缀、反斜杠、符号链接以及所有非常规类型；写者在落地之前按路径分量对着规范化后的目标目录重新检查一遍包含关系，所以一个事先埋在目标目录里的符号链接会在文件落到它下面之前被抓住。
- **一次安装是全有或全无。** 文件先解到 `<root>/.staging/<name>-<pid>`，只在 tar 解析通过、每个文件都算过摘要之后，才被移动到 `<root>/bundles/<name>`。失败的安装让根目录保持逐字节不变。
- **卸载只删记录里列出的东西。** 一条记录里的文件摘要变了、一条记录里的文件不见了、或者 bundle 目录里有一个记录从未列出的文件，三者中任何一个都会在删任何东西**之前**中止整次卸载，并点名那个出问题的路径——用户的改动不归市场来删。

`open_tarball` 是大多数调用方想要的那个唯一入口：它嗅探 gzip 魔数，然后要么先 gunzip 再解析，要么直接解析 `.tar`。

**`pack_dir` 是生产步骤，并且它共用同一套编解码。** 它遍历一个目录，写出带真实校验和的 ustar 头，再把结果包进一个由**stored** DEFLATE 块（不压缩）构成的 gzip member。重点不是压缩没有必要，而是打包与解包必须一致：归档由 `parse_tar` 校验的同一组常量产出，用索引校验器所用的同一个 `sha256_hex` 计算摘要，于是「pack 打印的摘要」与「install 校验的摘要」是同一个计算，而不是两个碰巧一致的实现。对体积小、且大多是本就已压缩文本的载荷来说，stored 块还顺带省掉了第二件要审计的东西——DEFLATE 的**编码器**。有一个测试把输出直接喂给 `install_bytes`。

`pack_dir` 在同样的规则上拒绝而不是猜测：符号链接不跟随（它可能指向任何地方，而产出的 tarball 也就不能描述被打包的东西），非常规文件被拒，空目录不算一个 bundle，路径要按读者自己的安全规则检查（相对、无 `..`、无反斜杠、无盘符前缀、无 NUL），所以「能打包的名字」永远是「能读回来的名字」。超过 100 字节的路径在 `/` 边界上拆进 ustar 的 `prefix`/`name` 字段，拆不开就拒绝。

**一个 bundle 可以是平台特定的。** `os` 与 `arch` 是可选成员，但它们必须同行：两个都缺席表示平台无关的源码 bundle（一个 tarball，所有平台通用），两个都在表示恰好针对那一对的原生构建，而只声明一半会被拒绝而不是取默认值——「windows，任意 arch」不是编译器能产出的东西。接受的写法就是 `.github/workflows/release.yml` 发布的那几种（`windows`/`linux`/`darwin`，`amd64`/`arm64`），不是 Rust 内部的目标名，所以市场不会拒绝本项目自己构建出来的产物。唯一性键是 name + version + os + arch，正因如此，一个版本的六个平台构建才可能同时存在。`resolve` 在挑版本之前先按宿主平台过滤，于是「你的平台没有构建」会点名**确实**发布了的那些平台，而不是伪装成「没有这个版本」；`resolve_for(name, requirement, &Platform)` 把平台当参数接收，测试就能在 Windows 上问 `linux/arm64` 会拿到什么。当一个源码 bundle 与一个原生构建都适配同一个平台时，解析拒绝而不是猜测操作员指的是哪一个。

## 公开面

`Index::parse` / `resolve(name, requirement)` / `resolve_for(name, requirement, &Platform)` / `versions_of` / `validate`；`Bundle`（带 `fits(&Platform)` 与 `platform_label()`）与 `Engines` 作为索引的线上类型；`Platform` / `host_platform()` / `KNOWN_OS` / `KNOWN_ARCH` 用于平台感知的解析；`INDEX_VERSION`。

`download(&Client, &Bundle, dest_dir) -> Result<PathBuf>` 与 `verify_file(&Path, &Bundle)`。

`Store`（`new`、`record_path`、`bundles_dir`、`staging_dir`、`bundle_dir`、`load`、`save`、`list`、`get`、`ensure_absent`、`ensure_no_stray_directory`、`digest_of_file`），配 `InstalledSet`、`InstalledRecord`、`InstalledFile`，以及常量 `RECORD_NAME` / `RECORD_VERSION` / `BUNDLES_DIR`。

`install`、`install_bytes`、`uninstall`、`list`、`modified_files`、`extract_to`，返回 `Installed` 与 `Removed`。

`open_tarball`、`parse_tar` 配 `Archive` / `Entry` / `MAX_ENTRIES`、`unpack` 配 `Unpacked`、`gunzip` / `is_gzip`、`inflate`。

`pack_dir(&Path) -> Result<Packed>`，其中 `Packed` 携带 gzip 过的 `tarball`、它的 `sha256` 与 `size`（也就是一条索引条目需要的两个值），以及排好序的、被写入的 `files`。

`sha256_hex`、`is_sha256_hex`、`digest_of_file`、`crc32`、`now_rfc3339`、`rfc3339_from_epoch`，以及判形状的路径/URL 谓词 `is_safe_name`、`is_safe_version`、`is_safe_relative_path`、`is_absolute_http_url`。

每一种拒绝前缀都以 `pub const ERR_*` 的形式住在 `error` 里，调用方（以及测试）就能点名它撞上的那条规则，而不是去匹配消息文本。

## 依赖与边界

`hearth-core` 提供 `Error`/`ErrorKind`/`Result`，`hearth-net` 提供 HTTP 客户端，`ring` 提供 SHA-256——三者都已在预算内，没有哪个是为这个 crate 新加的。`serde` 与 `serde_json` 在这里，是因为索引和记录都是 JSON。

刻意没有 `flate2`、没有 `tar`、没有 `sha2`。`inflate` 里的 DEFLATE 解码器覆盖 RFC 1951 的全部三种块类型（stored、fixed Huffman、dynamic Huffman）、全部 29 个长度码与 30 个距离码、码长字母表里的重复码 16/17/18，以及一个有界输出缓冲区；它不接受预置字典（gzip 没有这个字段），也不接受最后一个块之后的尾随字节。它对着参考编码器产出的向量做校验，其中一个是本机 `tar -czf` 出来的真实 blob，其摘要在测试里钉死，于是那条容易悄悄出错的 dynamic-Huffman 路径，是用没人编造的输出来验证的。tar 读者出于同样的理由是手写的：512 字节头、八进制与 GNU base-256 数字、GNU `L` 与 pax `x` 长文件名、ustar `prefix`，以及头校验和的两种写法。

HTTP 不在这里重新实现。`hearth_net::Client` 拥有重定向、TLS 信任根、deadline 和响应体上限；本 crate 只决定一个状态码、一个长度和一份摘要意味着什么。非 2xx 响应被报成点名状态码的拒绝，从不重试，也从不被当成传输失败。

这个窗口故意很窄：本 crate 不签任何东西，也不运行它装进来的东西。`engines` 是被记录，不是被强制。`pack_dir` 产出字节；它不托管这些字节，也不写索引——写索引是 CLI 的事，因为一个 URL 是发布者的决定，而本 crate 对 URL 没有意见。

## 不要做

- 不要加 `flate2`、`tar`、`sha2` 或任何归档 crate。手写解码器就是重点：一份预算、一种可被审计的行为，以及钉住真实数据流的测试。
- 不要把校验和不匹配变成警告、重试或安装。整个 crate 存在的意义就是让那一种情况不可能发生。
- 不要让 `pack` 拥有自己的格式。它必须写出 `parse_tar` 能读的东西，并用校验器所用的同一个 `sha256_hex` 算摘要，否则「先 pack 再 install」就变成两个必须靠人手保持一致的实现。往返测试把 `pack_dir` 的输出直接喂给 `install_bytes`，正是因为这个。
- 不要在打包时跟随符号链接，也不要存一个读者会拒绝的路径。名字读不回来的 bundle，是没人能安装的 bundle。
- 不要删记录里没列出的文件，也不要删一个摘要已经变了的有记录文件。拒绝永远是更便宜的错。
- 不要解析 semver 区间，也不要为 `*` 在多个匹配里挑一个版本。这里的错误答案是无声的，而拒绝不是。
- 不要给只声明一半的平台取默认值，也不要在「都适配同一平台」的源码 bundle 与原生构建之间挑一个。前者让同一份索引在不同机器上含义不同；后者跑的是操作员没选的代码。拒绝，并点名这个冲突。
- 不要在 tar 解析通过、每个文件都算过摘要之前把一次安装搬到位置上；半个装好的 bundle 比没有更糟。
- 不要把安装记录和文件分成两趟写。记录要么描述已经写下的东西，要么什么也不描述。