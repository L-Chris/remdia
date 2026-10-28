# macOS 接入设计草案

状态：首版实现依据。VideoToolbox 同步完成、CPU NV12 与 ABI 2.1 路线已实现；双架构应用迁移按第 7 节验收。

目标：让 macOS replayer 使用 remdia 的同一套组件图，覆盖播放、seek、缩略图、
字幕 PCM/MP3、封面与音频校验，构建和运行均不依赖 FFmpeg。
基于当前 ABI 2.1、Windows 接入分支及 macOS 15+ 的双架构发布流程设计。

## 1. 主要决策

| 项目 | 方案 |
| --- | --- |
| 平台 | macOS 15+；aarch64、x86_64 分别原生构建、测试和发布 |
| 通用组件 | 复用 native 包的 demux、音频 decoder、resample、gain、RGBA、MP3、mux、sink |
| 视频组件 | 新增独立的 macos.video.decode，由 VideoToolbox 实现 |
| 硬件策略 | 默认允许硬解；初始化失败可尝试系统软解；如实报告实际选择 |
| 首版帧接口 | CPU NV12，只读、拥有独立存储，沿用现有 WGPU 上传与色彩转换 |
| 首版协议 | 保持 ABI 2.1；使用 VT 同步完成模式，复用 submit/receive/drain/reset |
| 首版验收 | H.264、8-bit 4:2:0、逐行 SDR，MP4/MKV；通用音频与字幕链路 |
| 扩展能力 | HEVC SDR 按机器实际能力独立验收；不承诺所有 profile、位深与硬解组合 |
| 后续阶段 | ABI 2.2 异步等待、Metal/IOSurface 共享、P010/Main10/HDR、更多 decoder 插件 |

首版可以使用硬件解码，同时通过 CPU 平面交付图输出；硬解与 GPU 零拷贝是两个
独立目标。不把未经实现的零拷贝或 HDR 能力计入首版。

## 2. 组件图与模块边界

~~~mermaid
flowchart LR
    D[native.demux] --> A[native.audio.decode]
    A --> R[native.audio.resample]
    R --> C[CPAL / CoreAudio]
    D --> V[macos.video.decode / VideoToolbox]
    V --> N[CPU NV12]
    N --> W[WGPU / Metal]
    V --> F[native.video.rgba]
    F --> T[Thumbnail]
~~~

字幕继续使用独立的 demux → audio.decode → 16 kHz mono resample 图，按需调用
native.mp3.encode。播放器继续管理暂停、时钟、音量、音频设备恢复和画面呈现。

新增 plugins/macos，产物 libremdia_backend_macos.dylib，注册 macos.video.decode。
建议采用 Rust 组件与 RAII 状态管理，加一个私有的薄 C 适配层调用 Apple C API，
使用系统 SDK 的结构体定义。仅该 dylib 链接 VideoToolbox、CoreMedia、CoreVideo、
CoreFoundation；remdia 核心不引入 Apple 框架或 CVPixelBuffer 类型。

输入为现有 Packet/Track，保留 stream_id、generation、整数 PTS/DTS、time_base、
duration 和 keyframe。Rust 解析 avcC/hvcC 参数集，建立 CMVideoFormatDescription；
使用 CoreMedia 需要的长度前缀样本格式。Windows 的 Annex B 转换留在 Windows
插件内，不上移为公共 demux 行为。

默认播放规划器需要去掉 windows.video.decode 的硬编码。拟增加统一的 decoder
解析入口，按平台默认候选、输入 codec/configuration、硬件策略与 CPU 内存要求
创建实际可用的实例。生成的 GraphSpec 仍使用明确的组件 ID，可直接检查和修改；
Engine::graph 继续允许调用者显式选择其他插件。枚举到 codec 名称只代表候选，
实际支持以给定参数成功创建会话为准。

## 3. 首版复用 ABI 2.1

Apple 文档明确：DecodeFrame 的 EnableAsynchronousDecompression 与
EnableTemporalProcessing 两个标志都关闭时，解码完成、输出回调执行完毕后，
函数才返回。见 [DecodeFrame 文档](https://developer.apple.com/documentation/videotoolbox/vtdecompressionsessiondecodeframe(_:samplebuffer:flags:framerefcon:infoflagsout:))。

首版固定使用此模式，并由插件处理显示顺序。这里的“同步”描述 API 完成方式，
硬解选择由独立的 decoderSpecification 控制，不表示强制使用软件 decoder。

| 结果 | 首版语义 |
| --- | --- |
| Accepted | submit 已保留一个待解码包 |
| WOULD_BLOCK | 待处理包或输出容量暂满，本次未消费输入 |
| Output | 一帧已经安全就绪，进入既有预算与边队列 |
| NEED_INPUT | 需要后续包才能继续，包括满足 B 帧重排前瞻 |
| RECONFIGURED | 输出约束或最大帧字节数变化，先复验连接及预算 |
| EOF | 延迟输出与重排队列全部排空 |

submit 只保留有界输入，receive 在已有预算预留下执行一次同步解码、整理结果，
按需交付帧或请求后续输入。组件不会处于“输入关闭、只有未来异步回调才能推进”
却假装 NEED_INPUT 的状态。阶段 A 必须用真实 VT 验证这一约束。

所有调用仍在媒体工作线程，UI 和 CoreAudio 回调不等待 codec。首版无需扩展函数表、
改变 CPU 帧布局或升级 Windows 插件协议。后续真正启用异步标志时，再采用第 8 节
的显式 PENDING 协议，不把异步行为隐藏在现有状态中。

## 4. VideoToolbox 生命周期与排序

VT 会话归图工作线程所有，其 submit/receive/reset 调用串行执行。回调 context
使用地址稳定的 RAII 对象；即使系统在内部线程执行回调，也只写受保护的完成结果，
不访问图、播放器或 UI。回调入口单独捕获 panic 并记录错误，不跨 C 边界展开。
调用 VT 时不持有回调所需的锁，避免重入死锁。

每个被接受的访问单元具有不可变的 FrameTicket：session_id、generation、
decode_sequence、PTS/DTS 和资源额度。提交错误时 Apple 不会再回调，应立即归还
ticket；回调错误、丢帧和正常输出都必须恰好结算一次。首版限定一个样本对应一个
受支持的逐行访问单元。

### B 帧与显示顺序

Apple 明确说明回调不保证显示顺序，不能按回调到达顺序直接送入播放器，也不能只
对“此刻已经收到”的少量帧排序后立刻输出。
见 [VTDecompressionOutputCallback](https://developer.apple.com/documentation/videotoolbox/vtdecompressionoutputcallback)。

输入按 DTS/解码序号提交；每次同步调用返回后汇合其完成结果，再根据 codec 配置
得到的安全重排上界 R，维护按 PTS 排序的帧队列。满足安全释放条件才输出，EOF
时排空剩余帧。H.264 SPS 约束和 HEVC 重排参数的解析与保守回退规则，是实现前的
小型技术验证项。它需要覆盖缺少可选重排字段的输入，不能假设所有视频只有两个 B 帧。

帧容量必须覆盖 R 所需的最小前瞻；不能使用小于 R + 1 的固定槽数导致死锁。
若给定分辨率与重排深度超过资源策略，应在配置阶段明确拒绝或要求提高预算。
系统“当前在解码的最小 PTS”可辅助诊断，不能代替对尚未提交的后续 B 帧的判断。

### drain

drain 关闭新输入，结束会话的延迟输出，再将重排队列按 PTS 排空。
输出队列与重排队列都清空后才返回 EOF。

可以使用 [WaitForAsynchronousFrames](https://developer.apple.com/documentation/videotoolbox/vtdecompressionsessionwaitforasynchronousframes(_:))
建立结束屏障；该 API 已包含 FinishDelayedFrames，无需重复调用。
固定同步模式下逐次提交已经完成，首版不允许留下未被生命周期模型覆盖的异步任务。

### seek / reset

reset 清空待处理包和尚未交付的旧输出、重排状态，提升 generation，销毁旧会话并
建立新会话。demux 复用现有关键帧预滚，facade 复用目标时间裁剪。
已经交付的旧帧保持旧 generation；不能在新会话上给旧 ticket 重打标签。
重复 seek 继续由宿主合并，新请求可在下一次安全的 codec 调用边界抢占。

### close / cancel

在调用边界检查取消，安全终止会话后才释放 callback context 和剩余 ticket。
应用沿用现有后台回收路径，UI 不等待系统清理。保留的系统图像缓冲和已经输出的
CPU 帧各自拥有独立引用，不因销毁 decoder 失效。

不能承诺任意 Apple 系统调用可被强制中断。超时可以停止对外等待，但不能释放仍可能
被回调引用的内存；工作线程和回收任务必须持有上下文到真正完成。该行为需要纳入
关闭与快速切换测试，不能用标志位声称已经中止仍在执行的系统调用。

## 5. 帧格式、内存和硬解选择

首版请求双平面 NV12，识别 full-range / video-range；从实际 CVPixelBuffer 读取
宽高、stride、平面行数与像素格式，保留 clean aperture、SAR、色彩矩阵、范围、
primaries、transfer。不把编码对齐尺寸当作可见画面。

回调中先 retain 系统图像缓冲，同步调用返回后由所属工作线程完成只读 lock、逐平面复制、
unlock、release。公共输出持有 Rust 自有的只读平面，可在关闭 session 后继续使用，
并在任意消费线程 release。首版不把 Apple 对象指针塞进 CPU 字节平面。

decoder 依据协商后的尺寸声明 max_output_bytes；变化时先发 RECONFIGURED，再交付
对应帧。提交包数、压缩字节数、在途 surface、重排帧和就绪帧均有额度。回调不等待
UI 消费，也不在额度用尽后临时增长无界队列。

已交付 CPU 平面继续进入现有预算账本；VT 内部内存与保留的系统 surface 另设有界
策略并输出诊断，不宣称是整个进程 RSS 的硬上限。

硬件策略：

- Request.hardware=true 对应 Auto，允许 VT 选择硬解；会话创建失败时可尝试系统软解。
- Request.hardware=false 对应 Software，明确关闭硬解。
- 显式组件配置可提供 RequireHardware，失败时返回清楚的能力错误。
- hardware_decoded 根据实际会话属性查询，不能根据请求值推断。

Apple 的 [EnableHardware](https://developer.apple.com/documentation/videotoolbox/kvtvideodecoderspecification_enablehardwareacceleratedvideodecoder)
与 [UsingHardware](https://developer.apple.com/documentation/videotoolbox/kvtdecompressionpropertykey_usinghardwareacceleratedvideodecoder)
分别表达请求和实际选择。[RequireHardware](https://developer.apple.com/documentation/videotoolbox/kvtvideodecoderspecification_requirehardwareacceleratedvideodecoder)
也可能因资源繁忙或特定配置不支持而失败。

播放中发生硬件错误时，禁止直接切换到空白软解会话继续喂 P/B 帧。恢复须由宿主从
前一个可用关键帧重建图；无法安全恢复时明确报错。首版不引入隐藏的 FFmpeg 回退。

## 6. replayer 与 macOS 打包

macOS 默认选择已经在 Windows 验证过的 remdia 播放协调、音频输出、字幕和 renderer
路径。CPAL 继续使用 CoreAudio，WGPU 使用现有 Metal 路径。无需另写 AVPlayer，
也不让 AVFoundation 接管时钟和窗口。

当前 macOS 构建与打包有两处必须一起迁移：setup-macos.sh 构建 FFmpeg/LAME，
package-macos.py 读取 FFMPEG_DIR 并复制其动态库与源码。新流程改为构建 Cargo.toml
锁定的 remdia revision，打包 native + macos 两份 capability dylib。

拟定 bundle 布局：

~~~text
replayer.app/Contents/
  MacOS/replayer
  Frameworks/remdia/
    native.json
    macos.json
    libremdia_backend_native.dylib
    libremdia_backend_macos.dylib
  Resources/remdia/
    REVISION
    THIRD_PARTY.md
    licenses/
~~~

manifest 和 dylib 必须同目录，以满足当前 registry 的“library 仅为文件名”约束。
bundle 从 Contents/Frameworks/remdia 发现插件；固定 debug 预览从 target/debug/plugins
发现插件，开发时仍可显式指定 REMDIA_PLUGIN_DIR。

打包显式收集动态加载的插件，而不是只遍历主程序的链接依赖闭包。处理 bundle 内
相对依赖路径，先签 dylib 再签 app；保留原有 ad-hoc / Developer ID、ZIP、DMG、
校验和与公证流程。审计主程序和所有插件的架构、deployment target、签名、安装名
与依赖，拒绝 FFmpeg、Homebrew 和构建目录的外部依赖。

stage.py 的 arch 应由实际 target/Mach-O 切片决定，不能在指定 --target 后仍使用
构建主机的 platform.machine。分别发布两个架构；首版不增加 universal 包复杂度。
通用编解码库继续封装在 native dylib，随包更新对应许可、精确 revision 和源码/重建
指引，移除原先专属于 FFmpeg 的资源清单。

## 7. 实施顺序与验收

| 阶段 | 交付 | 完成条件 |
| --- | --- | --- |
| A：接口与技术验证 | ABI 2.1 下 VT 同步完成、硬解属性与排序小样 | 确认回调在调用返回前完成，B 帧安全排序与排空，无未建模的异步状态 |
| B：macOS decoder | dylib、能力选择、CPU NV12、硬件策略、seek/drain | 真实插件图解码 H.264 MP4/MKV，B 帧、帧生命周期、预览时间与排空通过 |
| C：replayer 接入 | 共用播放/字幕/渲染路径、音频恢复、固定预览 | 双架构的原生媒体测试通过，实际 CoreAudio 与 Metal 播放验证 |
| D：发布链路切换 | 新 setup/stage/bundle、清理旧后端 | 支持平台的 Cargo 依赖与 Mach-O 闭包无 FFmpeg，迁移包测试通过 |

阶段 D 完成后，replayer 移除 ffmpeg-next/ffmpeg-sys、旧媒体模块及其构建依赖。
对照测试使用旧提交的独立构建，不把旧后端保留为正式包内的运行时回退。

验收清单：

- aarch64 与 x86_64 的 H.264 MP4/MKV：72 帧 B 帧语料完整输出、PTS 顺序正确、
  暂停、快速连续 seek、seek 到 EOF、EOF 后 seek 与重播。
- 软件模式是稳定 CI 基线；Auto 模式记录实际硬解结果。RequireHardware 在可用的
  真实设备上另测，不能把虚拟 CI 的“无硬件”算成硬解已验收。
- HEVC SDR 按能力矩阵独立测试。10-bit/HDR、超预算配置或不支持的输入明确报错，
  不静默降位深。验证画面裁剪、非紧密 stride、SAR、BT.601/709 与 full/video range。
- WAV/FLAC/MP3/AAC/Opus、封面、16 kHz 单声道字幕范围裁剪、LAME MP3、
  音频设备恢复继续走通用 native 语料。
- HTTP Range、缓存未就绪、取消阻塞打开、seek 抢占、旧帧迟到、排空途中取消、
  回调错误/丢帧、格式变化与非显示顺序输出均有回归。
- 独立 C 宿主验证 ABI、旧版本拒绝、输入 retain/release、关闭组件后的帧访问，
  以及跨线程释放；加入 ASan/TSan 等可用工具的回调生命周期检查。
- 迁移、签名并移动 ZIP/app 后，以干净 PATH 且清除开发插件路径运行；
  测试 ZIP 与挂载 DMG，验证 CoreAudio、视频播放与字幕提取。
- 测试语料复用仓库文件，不要求安装 FFmpeg 生成测试素材。Mac/Windows CI 都通过
  后才完成默认后端切换。
- 按同架构、同构建设置比较安装包、解压 app、主程序、插件、源码/许可资源的体积；
  CPU、启动与 seek 延迟、硬解结果、内存分别实测，不提前承诺收益。

## 8. 后续异步与 Metal 零拷贝

性能实测后，可先在 CPU 平面接口下升级异步解码，再接 GPU 共享；两项独立验收。

异步阶段提议 ABI 2.2 增加 PENDING（拟状态 5），明确表达在途回调和排空等待。
函数表布局不变，新宿主兼容旧 2.1 插件，新插件拒绝较低 minor 的宿主。
图收到 PENDING 后仍可尝试提交输入并调度其他节点，仅全图无进展时短暂、可取消地
等待。异步 EOF 必须等待 drain-complete、所有回调与重排输出完成。
旧 generation 的迟到回调必须按原始 ticket 丢弃；取消后回收中的会话和上下文继续
计入额度。需要新增无忙轮询、乱序回调、回调迟到、排空取消及额度耗尽的模拟测试，
并重新验证 Windows 与独立 C 宿主。Session/CLI 等所有直接消费组件的路径均须识别
新状态。若阶段 A 发现同步模式无法达到验收目标，先评审这一扩展再修改首版范围。

Metal 阶段才引入可协商的 CPU / IOSurface / Metal 内存域、GPU 资源 owner、设备一致性、
同步与跨线程释放语义，配合 WGPU 纹理导入验证。需要 CPU 的 RGBA/缩略图滤镜通过
显式下载节点连接。GPU 对象不冒充当前字节平面，也不让所有滤镜隐式读回 CPU。

P010、Main10、HDR 色彩与 tone mapping 跟随这条独立的渲染与格式扩展路线验收。
它们不作为首版 CPU NV12 接入的隐含承诺。
