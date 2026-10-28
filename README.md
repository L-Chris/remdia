# remdia

**不依赖 FFmpeg 的模块化媒体框架。** Rust SDK 和 CLI 使用可组合的 demux / decoder / filter / encoder / mux / sink 插件图，支持 Windows Media Foundation 与 macOS VideoToolbox，供 replayer 共用同一套媒体图。

0.3 开发版将图片纳入同一组件图及 C ABI 2.2。`MediaRequest::Image` 和显式 `GraphSpec` 共用执行器、资源预算、取消和缓冲所有权：Windows 使用 WIC，macOS 使用 ImageIO/CoreGraphics。`image` capability pack 提供 decode/probe/scale/crop/alpha/orient/tile/compose/encode 工厂。Windows 缺少 WIC WebP codec 时，SystemPreferred 可选用独立的 WebP 解码插件，后续缩放/编码仍走系统组件。该窄回退当前仅接受无 ICC/EXIF metadata 的 WebP；带这些信息的文件需要系统 codec。

```sh
remdia capabilities
remdia image page.png --probe
remdia image page.png --width 1200 --output preview.jpg
remdia image page.png --width 1200 --plan
```

图片任务支持本地文件、共享编码字节和已有像素帧；显式图通过 `Engine::graph_with_inputs` 绑定 `core.buffer.source`。首批输出是有界 CPU RGBA8、straight alpha、sRGB；缩略图与音视频 seek 共用时间和 generation 语义。音频与容器的系统优先迁移仍在继续，见[整体方案](docs/system-media-and-image-design.md)。

## 架构

```text
                               ┌→ audio decoder → resample → audio output
file / HTTP → Rust demux ───────┤
                               └→ Windows MFT / macOS VideoToolbox decoder → native NV12 → renderer

file / HTTP → demux → audio decoder → gain → resample → WAV mux → file sink
                  └→ compressed packet copy → MP3 mux → file sink
```

组件通过稳定平台 C ABI v2.1 独立注册和创建。一个图中的节点可以来自不同 DLL；只有输入/输出类型、格式、采样率等约束兼容的连接才允许执行。运行时校验 DAG、逐轨路由、下游优先调度、有界边队列、EOF 排空和 seek generation。没有隐藏的 FFmpeg 回退。

| 能力包 | 独立组件 |
| --- | --- |
| `native` | Symphonia demux、音频 decoder、sinc 重采样/混音、音量滤镜、RGBA 缩放、WAV mux、LAME MP3 encoder、压缩 packet 复制、MP3 elementary mux、文件 sink |
| `windows` | Media Foundation 同步软件视频 decoder，输入压缩 packet，输出 CPU NV12 |
| `macos` | VideoToolbox 同步完成视频 decoder，硬解 Auto / 软件模式，输出 CPU NV12 |

常规音频由 Symphonia 和独立 libopus 提供：MP3、FLAC、WAV/PCM、AAC-LC、ALAC、Vorbis、Opus 等。AC-3、E-AC-3 与多声道 AAC 使用系统解码器：Windows 为 Media Foundation，macOS 为 AudioToolbox，支持路径取决于系统组件是否可用；不附带独立解码器兜底。Rust demux 支持 MP4/MOV、MKV/WebM、Ogg、WAV、FLAC、MP3、AIFF、CAF 的已识别流。Windows 视频首要验收路径是 MP4/MKV 的 H.264；HEVC/VP8/VP9 等取决于系统已安装的 MFT，不能仅凭扩展名承诺可播放。Symphonia 的视频参数支持仍使用其 experimental feature，版本固定在 Cargo.lock。

macOS 支持 macOS 15+ 的 Apple Silicon 与 Intel，H.264 SDR MP4/MKV 已验证；HEVC SDR 取决于系统能力，Main10/HDR/隔行输入明确拒绝。VideoToolbox 使用同步完成模式并按参数集声明的上界重排 B 帧；硬解请求与实际硬解结果分别记录。CPU 平面可跨线程使用，Metal/IOSurface 零拷贝留待后续。

**当前 Windows 视频使用同步软件 MFT。** 暂未接入异步硬件 MFT、D3D surface 共享或 HDR 色调映射；`hardware_decoded` 如实返回 false。该边界优先保证无需 FFmpeg 的组件拆分和播放器接入可验证。

## 构建与运行

Windows x64 需要 Rust stable、Visual Studio C++ Build Tools、CMake 和 Python 3。无需 FFmpeg、libclang、额外编解码 DLL 或 FFmpeg 开发包。

```powershell
./scripts/build-windows.ps1 -Release
./dist/remdia/remdia.exe plugins
./dist/remdia/remdia.exe probe 'D:\media\movie.mp4'
./dist/remdia/remdia.exe decode 'D:\media\movie.mp4' --frames 300
./dist/remdia/remdia.exe thumbnail 'D:\media\movie.mp4' thumb.png --at-us 1000000
./dist/remdia/remdia.exe extract 'D:\media\movie.mp4' speech.mp3 --start-us 30000000 --duration-us 30000000
./dist/remdia/remdia.exe graph examples/audio-to-wav.json
```

macOS 需要 Rust、Apple Command Line Tools、CMake 和 Python 3：

```bash
./scripts/build-macos.sh --release
./dist/remdia/remdia decode tests/fixtures/av.mp4 --frames 1000
```

产物按架构发布为 `remdia-macos-aarch64.zip` / `remdia-macos-x86_64.zip`。主程序与插件仅链接系统运行库；无需 FFmpeg 或 Homebrew 编解码动态库。

输出：`dist/remdia-windows-x86_64.zip`。Windows x64 配置静态链接 MSVC 运行库；CLI 和组件 DLL 无需另装 VC Redistributable。`plugins/` 同时包含 `native`、`windows` DLL 及 manifest。CLI 通过自身目录寻找插件，也可用 `--plugins PATH` / `REMDIA_PLUGIN_DIR` 指定。时间参数为整数微秒。

`graph` 接受用户定义的 JSON 图，支持串接、逐轨分支、扇出、滤镜顺序调整、绕过 decoder 的压缩流路径。文件 sink 不覆盖已有目标；写到临时文件，mux 排空后发布。WAV mux 在结尾通过带 offset 的输出修正 RIFF 头。MP3 packet mux 输出 elementary stream，重新封装的流不承诺保留原始 ID3/Xing/gapless 信息；MP3 encoder 则写入新的 LAME 延迟/填充标签。

`plan` 是不加载 DLL、不打开文件的结构探测计划；`Session::plan()` 是已探测轨道后实际执行的组件图。SDK 可通过 `Engine::graph()` 执行任意兼容图，不局限于播放场景。

## Rust SDK

```rust,no_run
use remdia::{Cancellation, Engine, Request};
# fn main() -> anyhow::Result<()> {
let engine = Engine::discover("plugins", 256 * 1024 * 1024)?;
// Native capability packs execute code and must be trusted.
let mut session = unsafe {
    engine.open(Request::playback("movie.mp4"), Cancellation::default())?
};
println!("{}", serde_json::to_string_pretty(session.plan())?);
while let Some(frame) = session.read()? {
    // FrameInfo describes native planes or f32le PCM. Buffers remain valid
    // after a session closes, and can be shared with the rendering thread.
    let _ = (frame.info(), frame.plane(0));
}
# Ok(()) }
```

SDK Session、图和组件留在创建它们的工作线程；只读帧与取消 token 可跨线程。每个节点使用 submit/receive 分离协议；RECONFIGURED 显式通知端口与输出预算变化，宿主复验连接后再获取新格式帧。WOULD_BLOCK 不消费输入。宿主以 retain/release 显式传递输入引用，跨 DLL 不传 Rust allocator 对象。

受管理预算覆盖图输出及边队列持有的缓冲、分段 PCM；队列还分别限制数量和媒体时长。预留不足时不会消费输入。跨组件包装可能保守重复计入共享数据，且编解码器内部、系统驱动、临时导出等不属于整个进程 RSS 的硬上限。没有相对 FFmpeg 的性能或包体收益承诺。

## 验证

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
python scripts/stage.py
python scripts/fixtures.py
$env:REMDIA_TEST_PLUGIN_DIR = "$PWD/dist/remdia/plugins"
$env:REMDIA_TEST_FIXTURES = "$PWD/target/fixtures"
cargo test --workspace --locked -- --include-ignored
python scripts/audit-package.py dist/remdia
```

语料已提交在 `tests/fixtures`，构建和测试均无需外部媒体工具。测试覆盖音频格式、范围裁剪、重采样与 gapless 尾部、MP3、封面、滤镜扇出、WAV finalize、预算、取消，以及 Windows 跨 DLL 的 packet → MFT → NV12 路径。CI 额外用独立 C 宿主检验 ABI，并在移除构建环境 PATH 后运行便携包。Windows x64、macOS aarch64/x86_64 是视频发布目标；Linux 可验证通用音频与图。

[架构](docs/architecture.md) · [插件 ABI](docs/plugin-abi.md) · [macOS 设计](docs/macos-integration-design.md) · [replayer 接入](docs/replayer-scope.md) · [原始架构提案](docs/remedia.md) · [第三方许可](THIRD_PARTY.md)
