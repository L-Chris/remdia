# remdia

Windows 优先的模块化媒体 SDK。参考 [remedia 架构提案](docs/remedia.md)，首版聚焦 [replayer](https://github.com/L-Chris/replayer) 当前使用的媒体能力。

Rust 核心负责任务规划、输出缓冲预算、取消与 seek generation；媒体后端通过版本化 C ABI 动态加载。首个能力包使用 FFmpeg 9，核心与 CLI 不链接 FFmpeg。**v0.1 是可运行的 SDK/CLI 起点，不是完整播放器，也不是 FFmpeg 的完整替代品。**

## 首版能力

| 能力 | 接口 / 行为 |
| --- | --- |
| 媒体探测 | 容器、轨道、编码、时长、起始时间、标签；不启动解码器 |
| 视频 / 音乐解码 | 本地文件、FFmpeg 支持的 HTTP(S) / Range 输入；按需返回帧 |
| Windows 硬解 | 尝试 D3D11VA，创建失败可回退软件；实际硬解状态逐帧报告 |
| 视频数据 | 原生 CPU plane、stride、SAR、色彩信息；缩略图单独转换 RGBA |
| 音频数据 | 重采样 / 混音为指定采样率的 f32le 单声道 / 立体声 |
| seek / EOF | 整数时间、解码预滚、音频样本裁剪、generation、解码器与重采样器排空 |
| 封面 / 缩略图 | 有界提取原始内嵌封面；按时间生成最大 600px RGBA 缩略图 |
| 字幕音频 | 独立音轨会话、16kHz 单声道有界分段、WAV、带 Xing/LAME 延迟和填充信息的 MP3 |
| 资源与取消 | 输出预算预留、保留帧背压、跨线程取消、I/O 超时、线程配置 |

格式是否可用取决于安装的能力包、FFmpeg 构建和设备，扩展名不代表支持承诺。HTTP seek 需要源端可 seek；下载缓存、鉴权、QQ 解密和 torrent 桥继续由应用提供。参见 [replayer 对照表](docs/replayer-scope.md)。

## Windows x64 构建

需要 Rust stable、Visual Studio C++ Build Tools、LLVM（含 `libclang.dll`）、Python 3 和 PowerShell。

```powershell
./scripts/build-windows.ps1 -Release
./dist/remdia/remdia.exe plugins
./dist/remdia/remdia.exe probe 'D:\media\movie.mkv'
./dist/remdia/remdia.exe plan 'D:\media\movie.mkv'
./dist/remdia/remdia.exe decode 'D:\media\movie.mkv' --frames 300
./dist/remdia/remdia.exe thumbnail 'D:\media\movie.mkv' thumb.png --at-us 10000000
./dist/remdia/remdia.exe extract 'D:\media\movie.mkv' speech.mp3 --start-us 30000000 --duration-us 30000000
```

打包输出：`dist/remdia-windows-x86_64.zip` 和 `dist/SHA256SUMS.txt`。编解码 DLL 与可执行文件同目录，插件位于 `plugins/`。脚本下载固定 SHA-256 的 FFmpeg 开发包；需要联网。`--plugins PATH` 或 `REMDIA_PLUGIN_DIR` 可指定受信任插件目录。时间参数均为整数微秒。

`decode` 是不带音视频设备的解码诊断工具；音频设备、播放时钟、WGPU 展示、暂停和音量由宿主负责。`plan` 读取 manifest 生成结构计划，不加载 DLL、不联网探测，也不宣称已验证某个文件或 GPU 的兼容性。

## Rust SDK

```rust,no_run
use remdia::{Cancellation, Engine, FrameInfo, Request};
# fn main() -> anyhow::Result<()> {
let engine = Engine::discover("plugins", 256 * 1024 * 1024)?;
let cancel = Cancellation::default();
// Native plugins execute code; load only a trusted capability pack.
let mut session = unsafe {
    engine.open(Request::playback("movie.mkv"), cancel.clone())?
};
while let Some(frame) = session.read()? {
    match frame.info() {
        FrameInfo::Video { planes, .. } => {
            // Upload native planes using their strides; the frame owns their lifetime.
            let _y_plane = frame.plane(0);
            let _stride = planes[0].stride;
        }
        FrameInfo::Audio { rate, channels, .. } => {
            // Feed f32le PCM into the application's bounded audio device queue.
            let _pcm = frame.bytes();
            let _ = (rate, channels);
        }
        _ => {}
    }
}
# Ok(()) }
```

在媒体 worker 上创建会话；`Session` 不跨线程共享，`Frame` 可只读共享到渲染线程，`Cancellation` 可从 UI 调用。帧可比会话活得更久。帧的 clone 共享一次预算预留；持有过多帧会在下一次读取前返回预算不足，不会等待唯一消费线程。seek 后宿主应丢弃旧 generation 的已排队帧。

## 验证

```powershell
. ./scripts/setup-windows.ps1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python scripts/stage.py
python scripts/fixtures.py --ffmpeg $env:REMDIA_TEST_FFMPEG
$env:REMDIA_TEST_PLUGIN_DIR = "$PWD/dist/remdia/plugins"
$env:REMDIA_TEST_FIXTURES = "$PWD/target/fixtures"
cargo test --workspace --locked -- --include-ignored
```

集成测试需要真实插件和生成语料，默认标记 ignored；Windows CI 会执行它们，并在清理 FFmpeg 构建路径后验证 ZIP。包含 B 帧计数、重采样排空、精确范围、MP3 gapless、非零时间原点、封面、缩略图、坏文件、预算和取消。CI 无可用 GPU，D3D11VA 实际硬解仍需 Windows 实机验收。macOS/Linux 可用于开发验证，首版发布目标仅 Windows x64。

## 边界与后续

- 首版是融合的 session 能力包；尚未拆成任意组合的 demux/decoder/filter/mux 插件图。
- 暂无转封装、通用视频编码、GPU 共享纹理、Media Foundation、运行中卸载插件。
- 原生视频保持色彩描述，不完成 HDR 色调映射和显示色彩管理。
- 预算限制输出 plane 预留和分段 PCM，不限制 FFmpeg/驱动私有内存或整个进程 RSS。框架不默认积压解码帧。
- 尚无相对等能力 FFmpeg 的性能、内存或包体收益结论。
- 未修改或迁移现有 replayer；[接入说明](docs/replayer-scope.md) 给出替换边界。

设计与 ABI 契约见 [architecture](docs/architecture.md)、[plugin ABI](docs/plugin-abi.md)。remdia 源码采用 MIT；编解码依赖遵循各自许可证，见 [THIRD_PARTY.md](THIRD_PARTY.md)。
