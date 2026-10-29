# remdia

不依赖 FFmpeg 的模块化媒体 SDK。音频、视频和图片共用 Engine、Registry、GraphSpec、组件 C ABI 2.2、缓冲所有权、预算、取消与 generation；demux / decoder / filter / encoder / mux / sink 可逐节点组合。

0.3 开发版优先使用 Windows Media Foundation / WIC 和 macOS AudioToolbox / AVFoundation / VideoToolbox / ImageIO / CoreGraphics。核心不链接媒体编解码库或平台框架；系统适配在独立能力包里。

## 系统优先

```text
本地文件 → 系统 demux ─┬→ 系统 audio.decode → 系统 audio.resample → f32 PCM
                       └→ 系统 video.decode → CPU NV12 → renderer
HTTP/渐进源/容器缺口 → compatibility demux → 同样的独立 decoder/filter

文件/共享字节 → image.decode → scale/crop/alpha/orient/tile/compose → image.encode
视频解码帧 → native.video.rgba → image.from_video → image.scale → image.encode
```

系统能力按容器、编解码格式、采样率和请求约束选择。损坏输入、取消、I/O 和预算错误直接返回；不会因为系统操作出错而重试软件编解码器。`Request.policy` / `ImageRequest.policy` 可设置 `SystemOnly`，拒绝兼容提供方。显式 GraphSpec 仍允许调用方选择具体组件。

| 能力包 | 职责与依赖 |
| --- | --- |
| `native` | 自研增益、RGBA 像素转换、简单 WAV/MP3 packet mux、packet copy、file sink；没有第三方媒体算法库 |
| `system_demux` | `windows.demux` / `macos.demux`，输出压缩包；本地完整文件，包括无正确扩展名的临时文件 |
| `system_audio` | 系统音频解码、流式重采样/声道矩阵；Windows 另提供独立 MP3 MFT encoder |
| `windows` / `macos` | 独立视频解码；Windows 同步软件 MFT，macOS VideoToolbox 硬解优先/软件模式 |
| `image` | WIC 或 ImageIO/CoreGraphics 的 probe/decode/scale/crop/alpha/orient/tile/compose/encode 工厂 |
| `fallback_demux` | Symphonia 仅用于系统容器缺口及现有 HTTP/Range/渐进 source 适配；不注册音频 decoder |
| `fallback_mp3_lame` | 仅 macOS，默认系统没有 MP3 encoder；Windows 使用系统 MFT + 独立 gapless mux |

已经移除 `image`、`fast_image_resize`、`image-webp`、rubato、libopus 和 libopus adapter 的运行时媒体依赖。Opus 在两端走系统 decoder。WebP 仅使用 WIC / ImageIO；系统未安装匹配 codec 时返回不支持，不携带第三方 WebP 解码器。Windows 的 Vorbis 可随系统扩展变化，运行时查询后再选择。

Symphonia 的 ADTS、MPA、FLAC reader 与 decoder 处于同一上游 crate，兼容 demux 的 Cargo 图仍会出现这些 crate；MPA decoder 功能开关已关闭，AAC/FLAC 仍编译上游 decoder 模块，本包不注册或调用音频 decoder。macOS 的 h264-reader/scuffle-h265 只读取参数集和重排上界，视频像素由 VideoToolbox 解码。[第三方与源代码说明](THIRD_PARTY.md)列出精确边界。

本地原生 FLAC 使用自有的压缩帧定位层，补齐 Windows Source Reader 返回任意字节块及 macOS 可变块定位的缺口；逐帧 CRC 校验，优先 SEEKTABLE，无表时二分搜索并维护有上限的稀疏索引。解码仍是独立 Windows MFT / AudioToolbox 节点，不引入第三方 codec，也不缓存整段 PCM。

系统路径已覆盖 MP3、AAC、FLAC、ALAC、WAV/常用 PCM、Opus，以及 macOS Vorbis；Dolby 取决于系统安装。不是所有 codec/profile、声道布局或历史格式都已认证。视频首批认证为 SDR 8-bit H.264 MP4/MKV；HEVC/VP8/VP9 依赖平台适配和系统能力，HDR、隔行、异步硬件 MFT 和共享 GPU surface 尚未接入。

## 构建与诊断

Windows x64：Rust、Visual Studio C++ Build Tools、Python 3；TLS 依赖的部分构建环境也需要 CMake。macOS 15+ 支持 Apple Silicon 与 Intel，需要 Rust、Apple Command Line Tools、Python 3。

```powershell
./scripts/build-windows.ps1 -Release
./dist/remdia/remdia.exe capabilities
./dist/remdia/remdia.exe plan 'D:\media\movie.mp4'
./dist/remdia/remdia.exe decode 'D:\media\movie.mp4' --frames 1000
```

```bash
./scripts/build-macos.sh --release
./dist/remdia/remdia capabilities
./dist/remdia/remdia --system-only plan tests/fixtures/av.mp4
./dist/remdia/remdia image page.png --width 1200 --output preview.jpg
./dist/remdia/remdia image page.png --probe
./dist/remdia/remdia image page.png --plan
./dist/remdia/remdia graph examples/audio-to-wav-macos.json
```

`capabilities` 查询实际系统能力；`plan` 打开源并显示实际提供方，`Engine::plan` 则只提供不加载 DLL 的结构计划。CLI 默认在自身目录下发现 `plugins/`，也可使用 `--plugins` 或 `REMDIA_PLUGIN_DIR`。时间参数为整数微秒。Windows 组件静态链接 CRT，包不需要独立 FFmpeg 或 Homebrew codec 动态库。

显式图示例保留兼容 demux，以演示跨能力包组合；默认 facade 优先系统 demux。文件 sink 不覆盖已有目标；WAV mux 在 drain 后修正 RIFF 头。MP3 packet copy/mux 不承诺保留原始 ID3/Xing；gapless 编码请求生成新的延迟/填充标签。Windows 的 `windows.mp3.encode` 是独立的系统原始 MP3 工厂，支持的输入档位以能力查询为准。

Windows 的 16 kHz mono gapless 导出使用 MFT 编码和 `native.mp3.gapless_mux` 封装。首轮编码用系统 decoder 校准延迟，缓存成功结果；元数据标记有效样本数并排除首尾填充。macOS 默认公开接口没有 MP3 encoder，因此仅 macOS 保留 LAME。[能力边界与验证方法](docs/system-mp3-encoding.md)。

## Rust SDK

```rust,no_run
use remdia::{Cancellation, Engine, ProviderPolicy, Request};
# fn main() -> anyhow::Result<()> {
let engine = Engine::discover("plugins", 256 * 1024 * 1024)?;
let mut request = Request::playback("movie.mp4");
request.policy = ProviderPolicy::SystemPreferred;
// Installed component libraries execute native code and must be trusted.
let mut session = unsafe { engine.open(request, Cancellation::default())? };
println!("{}", serde_json::to_string_pretty(session.plan())?);
while let Some(frame) = session.read()? {
    let _ = (frame.info(), frame.plane(0));
}
# Ok(()) }
```

图片入口 `MediaRequest::Image` 只生成、执行同一张公共 GraphSpec；没有独立图片执行器。`Engine::graph_with_inputs` 绑定文件、共享编码字节或像素输入。首批像素输出为 CPU RGBA8、straight alpha、sRGB；probe 返回原图尺寸、方向、帧数和格式，decode 归一化方向及色彩，编码支持 PNG/JPEG。

Session/Graph/组件留在创建它们的 worker，只读帧和取消 token 可跨线程。submit/receive 分离，WOULD_BLOCK 不消费输入，RECONFIGURED 在新格式分配前让宿主复验连接与预算。旧 generation 不会作为新结果交付。

受管理预算覆盖图输出、边队列与保留的共享缓冲；它不是整个进程 RSS 上限。系统 codec、框架、驱动和临时内部缓冲需要另测。默认源配置不会暗中先下载整个远程文件，HTTP/Range 继续使用已认证的兼容 source/demux，再连接系统 decoder。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
python3 scripts/stage.py
python3 scripts/fixtures.py
REMDIA_TEST_PLUGIN_DIR="$PWD/dist/remdia/plugins" REMDIA_TEST_FIXTURES="$PWD/target/fixtures" \
  cargo test --workspace --locked -- --include-ignored
```

语料已提交，执行构建和测试无需媒体生成程序。测试包括真实 DLL/dylib、独立 C 宿主、可迁移包、逐包解码、系统 codec 缺失、样本数/时间戳、SRC 尾部和声道混合、seek/reset/EOF、取消/预算、图片/旧缓存、扇出及 mux finalize。CI 将系统认证用例放入独立限时进程，以便发现驱动调用停滞。

[首轮包体/构建/内存测量](docs/system-media-measurements.md) · [架构](docs/architecture.md) · [ABI](docs/plugin-abi.md) · [系统媒体与图片方案](docs/system-media-and-image-design.md) · [replayer 接入](docs/replayer-scope.md) · [原始架构提案](docs/remedia.md) · [许可](THIRD_PARTY.md)
