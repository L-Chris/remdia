# Android 接入

Android 后端从 API 28（Android 9）起支持 `arm64-v8a`、`x86_64`。音频、视频和图片继续使用公共 Engine、GraphSpec、ABI 2.2、MediaBuffer、预算账本和取消协议。核心不链接 JNI 或 Android framework；JNI 只属于 Android 适配与 Java 宿主。默认 AAR 每 ABI 统一为 `libremdia_sdk.so`；详见 [统一链接](linking.md)。

## 能力与插件边界

| 工厂 | 实现与首版范围 |
| --- | --- |
| `android.demux` | MediaExtractor；本地路径、`content://`、可 seek 的 fd 区间；独立输出压缩包及 CSD、轨道、时间戳 |
| `android.audio.decode` | MediaCodec；运行时查询系统 codec，输出交错 F32LE PCM，最多 6 声道 |
| `android.video.decode` | MediaCodec；查询支持 flexible YUV 的系统 codec，复制可见区域为独立 CPU NV12 双平面；偶数宽高、8 bit SDR |
| `android.audio.convert` | remdia 的基础 PCM 声道混合、分块与时间换算；保留采样率，不含第三方 DSP |
| `android.audio.encode` | MediaCodec AAC-LC，8–48 kHz、单/双声道；不提供 MP3 编码或 AAC gapless 保证 |
| `android.mux` | MediaMuxer MP4/WebM；独立输入压缩包，暂存到有上限的私有文件，结束后按 64 KiB 输出 Bytes |
| `android.image.*` | ImageDecoder、Bitmap、Canvas；probe/decode/scale/encode 接系统，共用现有 crop/orient/alpha/tile/compose 节点 |
| `native.*` | 共用增益、像素转换、WAV/simple mux、packet copy 与不覆盖已有文件的 sink |
| `fallback.audio.resample` | **可选** SpeexDSP 1.2.1 standalone SRC；只在确实改变采样率时使用，单库中的可选工厂，纯系统包在编译时去除 |

没有内置 FFmpeg、Symphonia、LAME、软件 Vorbis、软件 WebP 或其他音视频/图片解码库。系统 codec 可以由 Android 自带软件实现，使用系统 API 不代表必然硬件加速。`capabilities()` 返回已接入的编码能力，`system_encode` 和 `codecs` 单独列出系统枚举结果；最终仍按具体轨道格式检查、配置 codec，不保证所有同名编码的 profile 都可用。

图片输出沿用 CPU RGBA8、straight alpha、sRGB，decode 应用方向，probe 返回原图尺寸、方向、帧数。JPEG/PNG 编码已接入；WebP/GIF/BMP 以及可用的 HEIF/AVIF 等解码取决于系统。API 28 没有 MIME 查询接口，能力列表只公布保守的基础格式。动画当前仅解码首帧，最多接纳 1024 帧的源。解码前检查源像素上限并设置目标尺寸，不先解码整张超大图再缩放。

## 采样率和纯系统包

Android 的默认播放请求使用 `audio_rate: 0`，即保留源采样率。设置 `audio_channels` 仍可混为单/双声道；普通播放无需引入 SRC。

Android 没有接入可用于离线 PCM → PCM 的公共系统 SRC。请求 16 kHz AI 音频、而源为 44.1/48 kHz 时，常规包生成 `demux → decoder → fallback.audio.resample` 图。`policy: system_only` 禁止此转换；源本来就是目标采样率或使用 `audio_rate: 0` 时正常工作。`--system-only` 包在编译时排除 SRC 工厂，显式图引用该工厂也会因缺失而失败。

AI 输入先显式请求 16 kHz 单声道，再使用既有 Rust `audio_chunk().wav()`，或将 PCM 接入独立 `native.wav.mux`。Android 不会为了 MP3 导出引入 LAME；需要压缩输出时可显式组合 AAC 编码和 MP4 mux。

## 构建与应用接入

构建需要 JDK 21、Android SDK platform 35、build-tools 35.0.0、NDK 28.2.13676358，以及两个 Rust Android target。SDK/JDK 可通过参数指定；环境变量 `ANDROID_HOME`、`ANDROID_NDK_HOME`、`JAVA_HOME` 也可用。仓库本地工具的默认位置是 `.deps/android-sdk` 与 `.deps/android-jdk/Contents/Home`。

```sh
rustup target add aarch64-linux-android x86_64-linux-android
python3 scripts/build-android.py --clippy
python3 scripts/build-android.py --system-only
python3 scripts/audit-android.py --aar dist/android/remdia-0.3.0.aar
python3 scripts/audit-android.py --aar dist/android/remdia-0.3.0-system-only.aar
# 可只打一个 ABI；--debug 生成调试包
python3 scripts/build-android.py --abi arm64-v8a
```

输出为 `dist/android/*.aar`、大小/耗时/SHA-256 报告及 `SHA256SUMS.txt`。AAR 含 Java facade、R8 保留规则、每 ABI 的可信插件清单和 `.so`，原生库以 16 KB LOAD alignment 链接。最终 APK/AAB 的压缩、对齐、ABI 拆分仍由应用构建决定；AAR 大小不等于 Pordo 安装包增量。应用构建参考 [Android 官方 16 KB 打包要求](https://developer.android.com/guide/practices/page-sizes#packaging)，未压缩原生库使用 AGP 8.5.1 或更高版本。

应用设置 `minSdk = 28`，把 AAR 放到 `app/libs` 后：

```kotlin
dependencies { implementation(files("libs/remdia-0.3.0.aar")) }
```

在应用进程初始化；媒体任务在工作线程执行，示例使用 Java：

```java
Remdia.initialize(context.getApplicationContext());
// 在同一个 worker 中打开、读取、seek 和关闭。
JSONObject request = Remdia.request(uri.toString())
    .put("task", "audio").put("audio_rate", 0).put("audio_channels", 2);
try (Remdia.Cancellation stop = new Remdia.Cancellation();
     Remdia.Session session = Remdia.open(request, Remdia.DEFAULT_BUDGET, stop)) {
    for (;;) {
        Remdia.Frame next = session.read();
        if (next == null) break;
        try (Remdia.Frame frame = next) {
            ByteBuffer pcm = frame.plane(0); // 只读；PCM 用 LITTLE_ENDIAN 解释
            consume(pcm, frame.metadata);
        }
    }
}
```

`Frame` 保留 native buffer，seek 或关闭 Session 后仍可读，直至自身 `close()`。**不可在 Frame.close 后使用之前取得的 ByteBuffer 视图**。Frame 可跨线程释放；Session/Graph 必须留在创建线程。取消 token 可从其他线程 `cancel()`，创建 token 必须在 `initialize()` 之后。所有句柄需显式关闭，不能依赖 GC 释放 native 帧。图片通过 `Remdia.image(encodedBytes, operations, budget, stop)` 执行同一张图；显式插件图通过 `Remdia.graph(graphSpec, budget, stop)`。

`content://` 需要应用已持有读取授权，底层必须提供可 seek 的文件描述符。Rust `Engine::open_fd` 和 Java `fd://42?offset=0&length=...` 在打开时复制 fd，调用者随后可以关闭原 fd；可指定 APK asset 等文件区间。不支持 pipe 或未知长度的远程 provider，当前不暗中复制整个输入。

## 生命周期、限制和内存

- 独立节点遵循 submit/receive/drain/reset。WouldBlock 不消费输入；系统格式变化先发 RECONFIGURED，宿主刷新预算后才接收新格式数据。EOS 后可以 seek，旧 generation 帧保留原所有权。
- 默认 Java 账本 96 MiB、单帧上限 32 MiB、单压缩包 8 MiB；图片源有 64 MP 防护，具体请求还可设置更小像素和输出限制。预算覆盖图缓冲与保留帧，**不是进程总内存上限**。JNI 临时数组、Bitmap、系统 codec/driver 私有内存不由账本硬限制。
- 首版为 CPU 路径，系统图像/YUV 会复制到独立 native 缓冲；没有 Surface、AHardwareBuffer 或 GPU 零复制。HDR/BT.2020 和高位深视频不在当前接纳范围。视频 SAR/旋转等显示几何的完整适配仍需后续接入验证。
- MediaExtractor 公开 PTS，不公开独立 DTS；轨道 `initialization.dts_available=false`，包的 DTS 暂以 PTS 占位。播放由系统解码器重排；**尚未认证含 B 帧视频的无损 packet-copy/remux**。不能据此宣称通用视频 remux 已完成。
- 第一版不接 HTTP/Range、DRM、字幕渲染、动画逐帧播放；不支持的 codec/profile 明确报错，不自动引入软件解码器。系统查询/同步调用中的取消不能保证立即打断框架内部操作。
- mux 为单次导出，有界临时文件默认 256 MiB、最多 1 GiB，完成后才流出容器数据，不支持边录边输出。文件 sink 通过 Android 的 `renameat2(RENAME_NOREPLACE)` 发布，避免 SELinux 禁止 hard-link，并保留并发不覆盖保证。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/test-android.py --aar dist/android/remdia-0.3.0.aar --serial emulator-5580
python3 scripts/test-android.py --aar dist/android/remdia-0.3.0-system-only.aar --serial emulator-5580
```

测试工具构建无 Gradle 的 instrumentation APK，真实安装并读取当前 AAR。它按清单识别是否含 SRC，分别验证源采样率纯系统路径和重采样路径。使用独立的测试应用，不修改应用之外的数据。同一设备串行运行；不同设备使用独立工作目录。`.github/workflows/android.yml` 对两个 ABI 构建并审计，在 API 28、35、35/16 KB 模拟器测试两种包；工作流已经添加，尚未推送运行。

多库基线结果见 [Android 首轮测量](android-measurements.md)，当前单库对比见 [统一链接实测](linking-measurements.md)。模拟器验证不能替代真实设备上的硬件 codec、长时间播放和高分辨率内存测试。
