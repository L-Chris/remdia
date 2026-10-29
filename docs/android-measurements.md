# Android 首轮测量（多库基线）

本文保留首次多库实现的原始数据；后续单库实现见 [统一链接实测](linking-measurements.md)。

2026-10-29，分支 `feat/android-system-media`，基于 `e6847d6`。环境为 Apple M6、macOS 27.0 ARM64、Rust 1.99.1、Temurin JDK 21、NDK 28.2.13676358；Java 编译 SDK 35，原生最低 API 28。以下均为本地结果，GitHub Actions 尚未运行。

## 包体

Release、LTO、strip，同时包含 arm64-v8a 和 x86_64，Zip Deflate level 9：

| 配置 | AAR 字节数 | AAR MiB | ARM64 原生库合计 | x86_64 原生库合计 |
| --- | ---: | ---: | ---: | ---: |
| 常规，含可选 SRC | 3,913,400 | 3.73 | 4,010,112 B | 4,408,128 B |
| 纯系统，移除 SRC | 3,312,720 | 3.16 | 3,415,936 B | 3,761,424 B |

去掉 SRC 后压缩包减少 600,680 B，约 0.57 MiB。其 ARM64 `.so` 为 594,176 B，x86_64 为 646,704 B；这些数字包含独立插件的 Rust/JSON/ABI 包装，不等于 SpeexDSP 算法本身大小。

AAR 含两个 ABI，不是 APK 或 replayer 的安装包。应用按 ABI 拆分、签名、压缩/不压缩和 ZIP 对齐后的最终下载/安装大小要另测。

审计覆盖两种包的全部 ELF：清单和实际库一致，LOAD 至少 16 KB 对齐，RELRO 不与后续可写区冲突，JNI/ABI 入口存在，DT_NEEDED 仅含允许的公共 Android 运行库。选定 Android packages 的 Cargo 依赖树没有 FFmpeg、Symphonia、LAME 或第三方图片解码器；纯系统包的清单和实际文件都没有 SRC。

最终包哈希见 [常规包报告](measurements/2026-10-29-android/package-full.json)、[纯系统包报告](measurements/2026-10-29-android/package-system-only.json)；每份设备报告同时记录被测 AAR 和实际 ABI 原生库的 SHA-256。

## 构建时间

| 构建 | 总耗时 | ARM64 构建及入包 | x86_64 构建及入包 |
| --- | ---: | ---: | ---: |
| 新 Cargo target 目录，双 ABI release | 14.733 s | 7.821 s | 6.411 s |
| 相同目录，源码不变，再次构建 | 1.677 s | 0.329 s | 0.535 s |
| 同缓存打纯系统包 | 1.572 s | 0.194 s | 0.278 s |

总耗时包含 Java 编译和 AAR 打包；每 ABI 列还包含压缩入包。新 target 目录为 `target/android-clean-20261029`，编译本次选定 Rust/C 依赖；SDK、JDK、Rust target 和 Cargo registry 已预先下载，未清理 OS 文件缓存，所以**不是首次配置机器或冷磁盘耗时**。这是一次测量，不能作为跨机器性能结论。随后单独执行双 ABI 严格 Clippy 加构建通过，耗时 5.031 s；这解释了最终 `package-full.json` 的 total_seconds，不能与纯增量构建混为一谈。

原始 [新目录报告](measurements/2026-10-29-android/build-clean.json)、[增量报告](measurements/2026-10-29-android/build-incremental.json)、[纯系统入包报告](measurements/2026-10-29-android/build-system-only.json)。可复测：

```sh
python3 scripts/build-android.py --build-directory target/android-new-measurement --report target/android/clean.json
python3 scripts/build-android.py --build-directory target/android-new-measurement --report target/android/incremental.json
```

## 运行与内存

三台 ARM64 模拟器、两种 release AAR，每组 **25 项通过**。页大小由测试进程 `sysconf` 实测并和预期值比较。每组运行 instrumentation 的同一测试进程，以 25 ms 周期和每项检查结束时采集 Debug.MemoryInfo；以下为**应用进程 PSS 的观测峰值**，单位 KiB：

| 模拟器 | 页大小 | 包 | 初始化前基线 PSS | 观测峰值 PSS | 用例执行时间 |
| --- | ---: | --- | ---: | ---: | ---: |
| Android 9 / API 28 | 4096 | 常规 | 12,078 | 31,965 | 2.012 s |
| Android 9 / API 28 | 4096 | 纯系统 | 12,143 | 31,898 | 2.325 s |
| Android 15 / API 35 | 4096 | 常规 | 18,552 | 74,064 | 1.855 s |
| Android 15 / API 35 | 4096 | 纯系统 | 18,188 | 76,231 | 2.274 s |
| Android 15 / API 35 | 16384 | 常规 | 20,625 | 86,358 | 2.048 s |
| Android 15 / API 35 | 16384 | 纯系统 | 20,811 | 85,810 | 2.188 s |

执行时间不含 Java/APK 构建、安装和启动等待。语料是短音频、小尺寸视频和图片，包含多次打开/关闭，不能代表持续播放稳态、4K 峰值或绝对内存上限；25 ms 采样可能漏过短暂峰值。PSS 包含 Java/测试宿主，未合计其他 codec 服务进程或 GPU/驱动的内存。原始报告还记录 native heap allocated bytes，它与 PSS 不是同一口径，不应相加。不同 Android 版本的框架、分配器和共享页不同，不能据此比较 OS 的内存优劣；这批样本也没有显示纯系统包一定更省运行内存。

25 项覆盖：系统能力查询、PNG/WebP、JPEG/PNG 编码与缩放/裁剪/方向、透明度与 EXIF、图片预算拒绝、内嵌封面、WAV/MP3/FLAC/Opus/Vorbis、EOF 后 seek/generation/保留帧、fd/content URI、SystemOnly 策略与源采样率、MP4/MKV 视频、视频经图片图生成缩略图、跨线程取消、独立 filter → AAC → MP4 → file sink，以及目标文件被抢先创建时不覆盖。常规包音频转为 16 kHz；纯系统包保留源采样率并使用基础声道转换。

设备原始报告：

- API 28：[常规](measurements/2026-10-29-android/api28-4k-full.json)、[纯系统](measurements/2026-10-29-android/api28-4k-system-only.json)
- API 35 / 4 KB：[常规](measurements/2026-10-29-android/api35-4k-full.json)、[纯系统](measurements/2026-10-29-android/api35-4k-system-only.json)
- API 35 / 16 KB：[常规](measurements/2026-10-29-android/api35-16k-full.json)、[纯系统](measurements/2026-10-29-android/api35-16k-system-only.json)

宿主全 workspace 的 fmt、严格 Clippy、单元测试通过；两个 Android ABI release 构建和目标平台严格 Clippy 通过；macOS 真原生图片 3 项、媒体 10 项回归通过。x86_64 已交叉编译和审计，本地未运行该架构模拟器；新增 CI 会覆盖其 API 28/35/16 KB 测试。真实 Android 设备硬件 codec、长时间播放、超大图片和高分辨率内存/功耗仍需后续认证。
