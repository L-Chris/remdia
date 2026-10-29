# 系统媒体迁移的首轮测量

日期：2026-10-28。对比 main `4a7aee732d672a089512d0ee9efa28bf22fb4fa0`
与系统媒体分支 `747a10c7d3e6cc733e45c8ee8e2aa26d4b4ca0f3`。
后续 Windows FLAC 初始化修正不在这一轮构建产物中；本报告不把它当作已重新测量。
原始结果在 [measurements/2026-10-28-macos](measurements/2026-10-28-macos)。

## 条件

同一台 Apple M6、12 核、16 GiB、macOS 27.0 ARM64，Rust 1.99.1；
MACOSX_DEPLOYMENT_TARGET=15.0。两边分别使用全新 CARGO_TARGET_DIR、相同
`cargo build --workspace --release --locked --offline -j4`。依赖已下载，系统文件缓存温热；
不是首次联网安装，也不是从同一套已编译缓存增量构建。构建仅各一次，后台仍有日常进程，
不能据此承诺跨机器或稳定百分比收益。

用 [measure-macos.py](../scripts/measure-macos.py) 约每 50 ms 采样子进程树，
记录 RSS 与 libproc physical footprint；同时保存 `/usr/bin/time -l` 的前台进程最大 RSS。
树 RSS 会重复计共享映射，采样可能漏掉瞬时峰值，不能当作硬内存上限。
两次构建均没有观测到 swap 增长。

## SDK 干净构建和完整发行包

| 指标 | 原 main | 系统媒体版 |
| --- | ---: | ---: |
| Release 构建墙钟 | 40.986 s | 36.243 s |
| 构建树采样峰值 RSS | 1292.06 MiB | 1226.77 MiB |
| 构建树采样峰值 physical footprint | 888.79 MiB | 887.49 MiB |
| CLI + 全部 dylib 文件大小合计 | 8.12 MiB | 10.04 MiB |
| 完整 ZIP（相同 deflate level 9） | 3.91 MiB | 4.74 MiB |
| dylib 数量 | 2 | 7 |

完整包变大，原因包括新增图片与系统适配包、拆包后的公共支持代码，以及保留的
HTTP/容器兼容 demux 和 LAME 回退。旧包没有图片能力，两个包的功能集合并不完全相同。
不能把本表解释为“系统编码器替代前后等功能包”的纯算法对比。

原 native dylib 为 6,626,016 字节；新 native 工具包为 613,584 字节，但其余能力分散到了
独立 dylib，不能只拿这个减少量代表完整下载包。兼容 demux 仍为 5,341,664 字节，
它还包含网络/TLS 等 source 支持；Symphonia 上游部分格式 reader 和 codec 同 crate，
这里不宣称这些 codec crate 已从编译图消失。没有删掉 replayer 所需的渐进播放来美化数字。

## 长 PCM 解码

输入：用 Python `wave` 写出的 600 秒、44.1 kHz、双声道、s16le WAV；固定交替样本
0/1，输入共 105,840,044 字节。两边使用默认输出 48 kHz 双声道，运行：

```bash
./remdia decode pcm600.wav --frames 100000 --software
```

每边先测一次首次运行，再交替顺序测三次温热运行。CLI 均到达 EOF；图返回的缓冲批次
大小不同，因此不把 audio_frames（缓冲数量）当作 PCM 样本数量比较。

| 指标 | 原 main | 系统媒体版 |
| --- | ---: | ---: |
| 首次运行墙钟 | 2.722 s | 2.200 s |
| 后续三次运行墙钟中位数 | 1.726 s | 0.368 s |
| 后续三次树峰值 RSS 中位数 | 13.34 MiB | 40.33 MiB |
| 后续三次树峰值 physical footprint 中位数 | 4.19 MiB | 17.94 MiB |
| 后续三次前台最大 RSS 中位数 | 12.16 MiB | 39.08 MiB |

这个输入上系统路径吞吐更高，但加载 AVFoundation / AudioToolbox 等框架使运行内存上升。
PCM 长输入不能代表有损编码、图片、视频 GPU 内存或播放器闲置占用；内存数也不能外推为
同时打开多个媒体任务的线性增量。Graph 的 managed_peak_bytes 是缓冲预算指标，
与进程 RSS/physical footprint 分开理解。

## 复现和后续

```bash
python3 scripts/measure-macos.py --label sdk-release --output target/measure/build.json \
  --cwd . -- env MACOSX_DEPLOYMENT_TARGET=15.0 \
  CARGO_TARGET_DIR=target/measure/build cargo build --workspace --release --locked --offline -j4
python3 scripts/stage.py --profile release --build-directory target/measure/build \
  --output target/measure/runtime
python3 scripts/measure-macos.py --label pcm --output target/measure/pcm.json \
  --cwd . -- target/measure/runtime/remdia decode pcm600.wav --frames 100000 --software
```

脚本会自己创建 `.log` 与 `.time`；不要把脚本 stdout 重定向到它自己创建的 `.log`。
内存和 swap 增长保护只用于测量进程树，不是 SDK 运行时承诺。

本轮没有可比的 Windows 构建/内存数字，也没有 replayer 整体干净构建前后或长时间 UI
闲置内存数字。Windows/macOS 功能测试通过与性能收益是不同结论；后续需要在同一台
Windows 机器上固定输入、编译并行度和缓存条件独立测量。Blaze 未引入。
