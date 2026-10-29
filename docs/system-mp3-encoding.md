# 系统 MP3 编码能力

Windows 提供系统 MP3 encoder。当前 Windows-2025 实测接受单声道
16、22.05、24、32、44.1、48 kHz 配置，且 16 / 32 kHz 均完成实际编码与系统解码回读。
不能将当前 gapless 导出适配未完成，解释成系统不支持 16 kHz。
具体机器的可用档位仍以运行时 `mp3_input_rates` 查询为准。

macOS 的公开 Core Audio 默认可解码 MP3，但不提供 MP3 编码器。本地 AudioToolbox
encoder 格式枚举也未包含 MP3；存在 MP3 格式常量不代表具有对应 encoder。

## 原始编码与 gapless 契约

`windows.mp3.encode` 是独立图节点，接收 PCM 并输出原始 MP3 帧。
高层导出将它接到独立的 `native.mp3.gapless_mux`，补充 encoder delay / end padding
和 Xing/LAME-compatible 文件标签。能力结果里的 `mp3_gapless` 描述 SDK 已适配的
16 kHz mono 契约，不是 Windows 自带的系统能力标志。

2026-10-28，Windows-2025，2 秒单声道信号，使用原始系统 encoder 和当前系统解码链回读：

| 采样率 | 输入样本 | 回读样本 | 差值 |
| --- | ---: | ---: | ---: |
| 16 kHz | 32,000 | 33,456 | +1,456 / 91 ms |
| 32 kHz | 64,000 | 66,288 | +2,288 / 71.5 ms |

这是适配前的原始流基线。差值包含编解码链的首尾开销，不能直接当作 encoder delay。

高层 `encode_mp3` 约定 16 kHz mono、gapless。Windows 的 `SystemPreferred` 和
`SystemOnly` 都使用系统 MFT 编码及独立 mux，不构建或分发 LAME。
macOS 默认公开接口没有 MP3 encoder，因而仅 macOS 保留可替换的 LAME 插件；
macOS 的 `SystemOnly` 仍明确拒绝 MP3 编码。

## Windows gapless 适配

首次请求用确定性的宽带信号经过系统 encoder / decoder，以相关性测出信号延迟，
同时计数原始 MPEG 帧和解码样本，区分 decoder 已隐式去掉的 priming。
成功结果在进程内缓存；不写死某个厂商的 encoder delay。
校准不确定、被取消或失败时返回错误，不转用软件 encoder。

编码后补充 1152 个 guard 样本以排空末尾滤波器，这些样本不计入有效时长。
独立 mux 根据原始帧数、校准 delay 和调用方有效样本数计算 padding，写入
Xing 帧数 / 字节数 / seek TOC 及带 CRC 的 LAME-compatible delay/padding 标签。
`LAMErmdia` 是兼容元数据格式标识，不表示引入了 LAME 实现；ID3 `TENC`
明确标注真实 encoder 为 `Windows Media Foundation / remdia`。

认证覆盖 1、31、575、576、577、1024、17968、32000、32001 和 1920000 个样本，
检查有效时长、准确回读数量、随机信号相关性与末尾保留。高层只接受最多 120 秒的
完整 PCM 片段；任意时长的流式 MP3 编码不是当前高层契约，原始 encoder 节点
仍可由调用方独立组合。

同一 Windows 输出已交由 macOS AudioToolbox 解码，并分别经过 AVFoundation
和 Symphonia reader 读取标签：1、577、32001 个有效样本均精确保留，32001 样本
信号的零偏移相关性为 0.980169。固定样本保存在 `tests/fixtures/windows-gapless/`，
由两端 CI 持续验证，不依赖本项目自己的标签 parser 来证明兼容 reader 正确。

## Symphonia 边界

兼容 demux 直接注册所需 format / metadata reader，不依赖上游默认 codec registry。
Vorbis decoder 已从 workspace 和 lockfile 移除。
ADTS、FLAC reader 和 decoder 共享上游 crate，所以构建时仍会编译
`symphonia-codec-aac`、`symphonia-bundle-flac` 中的 decoder 模块。
`symphonia-bundle-mp3` 仍提供 MPA reader，但已关闭 `mp1/mp2/mp3` decoder
功能开关，只编译 demux 及共享辅助模块。没有注册、创建或调用软件音频 decoder。

macOS ARM release 链接图检查显示 `AacDecoder`、`FlacDecoder`、`MpaDecoder`
及 codec registry 的相关符号均不在保留段，decoder 实现被列在 dead-stripped 段。
FLAC 帧头的 UTF-8 整数读取等 demux 所需辅助函数仍保留；不能仅凭函数含有
`decode` 就认定它在解码音频。该检查证明此配置的链接结果，不推断所有平台
和构建选项都完全不含相关字节。复现：

```sh
cargo rustc -p remdia-backend-fallback-demux --release -- \
  -C link-arg=-Wl,-map,/absolute/path/to/remdia/target/fallback-demux-link.map
```

- [Windows MP3 encoder 官方说明](https://learn.microsoft.com/en-us/windows/win32/medfound/mp3-audio-encoder)
- [Apple Core Audio Services](https://developer.apple.com/library/archive/documentation/MusicAudio/Conceptual/CoreAudioOverview/WhatsinCoreAudio/WhatsinCoreAudio.html)
- [实测数据](measurements/2026-10-28-windows-mp3.json)
- [实际 DLL 编解码认证](https://github.com/L-Chris/remdia/actions/runs/40784141822)
