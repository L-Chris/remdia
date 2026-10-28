# 系统 MP3 编码能力

Windows 提供系统 MP3 encoder。当前 Windows-2025 实测接受单声道
16、22.05、24、32、44.1、48 kHz 配置，且 16 / 32 kHz 均完成实际编码与系统解码回读。
不能将当前 gapless 导出适配未完成，解释成系统不支持 16 kHz。
具体机器的可用档位仍以运行时 `mp3_input_rates` 查询为准。

macOS 的公开 Core Audio 默认可解码 MP3，但不提供 MP3 编码器。本地 AudioToolbox
encoder 格式枚举也未包含 MP3；存在 MP3 格式常量不代表具有对应 encoder。

## 原始编码与 gapless 契约

`windows.mp3.encode` 是独立图节点，接收 PCM 并输出原始 MP3 帧。
当前适配未补齐可靠的 encoder delay / end padding 信息及相应 gapless 文件标签。
能力结果里的 `mp3_gapless: false` 描述 SDK 当前保证的契约，不是 Windows 的系统能力标志。

2026-10-28，Windows-2025，2 秒单声道信号，使用原始系统 encoder 和当前系统解码链回读：

| 采样率 | 输入样本 | 回读样本 | 差值 |
| --- | ---: | ---: | ---: |
| 16 kHz | 32,000 | 33,456 | +1,456 / 91 ms |
| 32 kHz | 64,000 | 66,288 | +2,288 / 71.5 ms |

这证明原始流当前没有满足有效样本数准确保留的 gapless 导出要求。差值包含编解码链的
首尾开销，不能将它直接当作 encoder delay；后续适配还需区分起始延迟和末尾填充，
写入正确的文件元数据，并验证首尾信号、短片段和不同长度的回读结果。

高层 `encode_mp3` 当前约定 16 kHz mono、gapless。`SystemPreferred` 因此仍选择已验证的
LAME 实现；`SystemOnly` 明确拒绝当前未支持的完整契约。调用方可以在显式图中使用
系统原始编码节点，但不能将其输出直接声明为已经满足高层 gapless 契约。

Windows 后续可以继续补齐系统 encoder 的 gapless 封装适配；macOS 在保留 MP3 输出格式
时仍有默认系统 encoder 缺口。LAME 当前保留，不应将两个平台的保留理由混为系统采样率限制。

- [Windows MP3 encoder 官方说明](https://learn.microsoft.com/en-us/windows/win32/medfound/mp3-audio-encoder)
- [Apple Core Audio Services](https://developer.apple.com/library/archive/documentation/MusicAudio/Conceptual/CoreAudioOverview/WhatsinCoreAudio/WhatsinCoreAudio.html)
- [实测数据](measurements/2026-10-28-windows-mp3.json)
- [实际 DLL 编解码认证](https://github.com/L-Chris/remdia/actions/runs/40784141822)
