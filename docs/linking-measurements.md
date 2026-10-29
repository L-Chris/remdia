# 统一链接实测

测量于 2026-10-29，在 Apple M6 / macOS 27.0 上构建。Android 为两个 ABI 的 release AAR，macOS 为 ARM64 release。基线来自本次改动前保存的实际构建产物，未删除功能或字体，也未改为 panic=abort。

| 产物口径 | 多库基线 | 统一链接 |
| --- | ---: | ---: |
| Android 双 ABI 常规 AAR | 3,913,400 B | 1,761,753 B（−54.98%） |
| Android 双 ABI 纯系统 AAR | 3,312,720 B | 1,723,348 B（−47.98%） |
| macOS SDK CLI + 全部原生库，展开 | 10,575,760 B | 8,379,312 B（−20.77%） |
| replayer ARM64 主程序 + 全部媒体库，展开 | 32,494,656 B | 25,545,616 B（−21.39%） |

Android 每 ABI 从五个原生库合为一个；SRC 的增量因此也只剩算法和工厂本身，避免再带一套公共依赖。macOS SDK 含薄 CLI、一个 SDK dylib 和独立 LAME；replayer 把内置组件直接链接到主程序，只保留 LAME 扩展。应用主程序自身会变大，表中以主程序加媒体库的总量比较。

同样仅对上述二进制负载做 Deflate level 9，SDK 从 4,951,729 B 到 3,973,103 B，replayer 从 15,290,989 B 到 12,071,925 B。这是统一压缩口径，不是完整安装包。当前 macOS 测试 ZIP 为 12,735,312 B，DMG 为 14,580,995 B；ZIP 已验证解压搬迁后的签名与真实播放，DMG 校验通过、未做挂载测试。基线与当前应用都使用同一份本地 SQLite 裁剪配置，不能把这组数据当作线上正式版本的安装包对比。

## 构建与内存

Android 最终源码在新 target 目录中双 ABI release 构建耗时 **17.572 s**，相同目录再次构建 **0.805 s**。此前多库基线为 14.733 s / 1.677 s：单库这次干净构建较慢、增量打包较快，没有将包体收益描述为构建必然提速。这是单次测量，工作区并非隔离性能实验；不含工具链和 registry 下载，OS 文件缓存未清空。原始报告：[新目录](measurements/2026-10-29-linking/android-final-clean.json)、[增量](measurements/2026-10-29-linking/android-final-incremental.json)。

同一台 Android 15 / 4 KB ARM64 模拟器，交替运行多库/单库常规包各 3 次相同的 25 项用例。应用 PSS 观测峰值中位数为 **72,718 → 70,899 KiB**（约 −2.5%）。macOS 同一份视频的无界面播放/seek 自检，交替各 3 次；进程树 RSS 峰值中位数 **39,731,200 → 37,175,296 B**，physical footprint 峰值中位数 **12,583,968 → 12,321,848 B**。RSS 与 footprint 口径不同，不能相加；数据说明运行内存没有随磁盘体积减少一半，短语料也不足以代表长时间或高分辨率稳态。

Android 数据以 25 ms 及用例边界采样；macOS 使用已有 `measure-macos.py`，20 ms 采样，命令为两版 `replayer --selftest tests/fixtures/av.mp4`，使用各自匹配的插件目录和隔离设置文件。最终复验使用官方 platform-tools 36.0.2；本机的 37.0.1 ADB server 曾因 `libunwind` 异常退出，改用 36.0.2 后全部完成。这是宿主工具观察，不是播放器崩溃。工具来源可核对 [Google 平台工具预编译记录](https://android.googlesource.com/platform/prebuilts/fullsdk-darwin/platform-tools/+/363dd67470a21f3308919d41d1236bfe6c292f89)。

原始二进制、包体、测试包哈希与中位数汇总：[summary.json](measurements/2026-10-29-linking/summary.json)。完整原始单次报告与 ELF 审计保留在同目录，每份 Android 报告记录实际被测 AAR 和原生库的哈希。

Android 首轮多库的原始测量见 [基线报告](android-measurements.md)。新构建目录的耗时不包含 SDK/JDK/Rust target/registry 下载，不是冷磁盘或首次配置机器测试。集中 LTO 会改变构建并行度，不能仅凭库数量推断构建必然变快。后续原始构建、内存和认证数据保存在 `docs/measurements/2026-10-29-linking/`。

功能检查覆盖内置 Rust 路径、独立 C SDK 宿主、既有组件 ABI 宿主和外部 LAME 插件。Android 在 API 28、35、35/16 KB 上测试常规和纯系统包，每组 25 项通过；x86_64 交叉编译、严格 lint 与 ELF 审计由本机构建覆盖，运行时留给 CI。remdia 原生图片/媒体 13 项与系统组件认证通过；replayer 148 项普通测试、8 项真实媒体接入测试通过。两个仓库均通过严格 Clippy。Windows 构建/打包/升级清理与测试流程已经更新，本机没有 Windows 运行环境，未报告其包体或运行内存数字，也未推送触发 CI。

进程 PSS/RSS/physical footprint 是采样值，不能视为内存硬上限，也不能从磁盘大小降幅推导内存同比下降。系统 codec、Java/Bitmap、GPU 和外部服务的占用需按各自口径单独考虑。
