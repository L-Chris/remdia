# 统一链接实测

测量于 2026-10-29，在 Apple M6 / macOS 27.0 上构建。Android 为两个 ABI 的 release AAR，macOS 为 ARM64 release。基线来自本次改动前保存的实际构建产物，未删除功能或字体，也未改为 panic=abort。

| 产物口径 | 多库基线 | 统一链接 |
| --- | ---: | ---: |
| Android 双 ABI 常规 AAR | 3,913,400 B | 约 1.68 MiB |
| Android 双 ABI 纯系统 AAR | 3,312,720 B | 约 1.64 MiB |
| macOS SDK CLI + 全部原生库，展开 | 10,575,760 B | 约 8,331,008 B |
| replayer ARM64 主程序 + 全部媒体库，展开 | 32,494,656 B | 约 25,541,520 B |

Android 每 ABI 从五个原生库合为一个；SRC 的增量因此也只剩算法和工厂本身，避免再带一套公共依赖。macOS SDK 含薄 CLI、一个 SDK dylib 和独立 LAME；replayer 把内置组件直接链接到主程序，只保留 LAME 扩展。应用主程序自身会变大，表中以主程序加媒体库的总量比较。

Android 首轮多库的原始测量见 [基线报告](android-measurements.md)。新构建目录的耗时不包含 SDK/JDK/Rust target/registry 下载，不是冷磁盘或首次配置机器测试。集中 LTO 会改变构建并行度，不能仅凭库数量推断构建必然变快。后续原始构建、内存和认证数据保存在 `docs/measurements/2026-10-29-linking/`。

功能检查覆盖内置 Rust 路径、独立 C SDK 宿主、既有组件 ABI 宿主和外部 LAME 插件。Android 在 API 28、35、35/16 KB 上测试常规和纯系统包；x86_64 交叉编译、严格 lint 与 ELF 审计由本机构建覆盖，运行时留给 CI。Windows 构建/打包/升级清理与测试流程已经更新，本机没有 Windows 运行环境，未报告其包体或运行内存数字。

进程 PSS/RSS/physical footprint 是采样值，不能视为内存硬上限，也不能从磁盘大小降幅推导内存同比下降。系统 codec、Java/Bitmap、GPU 和外部服务的占用需按各自口径单独考虑。
