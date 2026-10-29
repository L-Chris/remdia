# 系统能力优先与音频、视频、图片统一组件图方案

状态：0.3 实施记录，2026-10-28。图片、系统解码/重采样和本地系统解复用已接入同一图；默认按节点选择。HTTP/Range/渐进 source 保留已认证的兼容 demux，再连接系统 decoder。
当前实现、能力/档位限制与剩余第三方边界以 [README](../README.md)、[第三方说明](../THIRD_PARTY.md)为准；下文保留设计动机与分阶段验收要求。
基线：remdia `4a7aee7`，replayer `462f908`；目标 Windows x64、macOS arm64/x86_64（当前 macOS 最低版本 15）。

## 目标与边界

remdia 统一封装音频、视频和图片的格式探测、解复用、解码、转换、编码与封装。
replayer 只引用 remdia 的媒体数据类型/API，移除自己直接使用的 `image`、`fast_image_resize` 和 `replayer-image`。
系统可用且满足输入/输出约束的组件优先；只有经支持矩阵确认的缺口才交给窄功能第三方插件。
核心继续不链接平台媒体框架或第三方编解码实现，不引入 FFmpeg。

音频、视频、图片共用同一个 Engine、Registry/Planner、GraphSpec/Graph、Component/Factory、插件 C ABI、MediaBuffer、Budget 和 Cancellation。
图片作为同一套 source / demux / decoder / filter / encoder / mux / sink 图里的媒体类型扩展，组件同样可替换、组合、分支和扇出。
不单设 ImageEngine、图片插件加载器、图片执行器、图片任务队列或绕过图的系统 API 服务。
便捷接口只能生成、执行公共 GraphSpec，不能直接调用 WIC/ImageIO；同一任务能通过显式组图完成，使用相同的资源与生命周期规则。
replayer 继续负责 UI、WGPU 纹理上传/呈现、CPAL 设备输出、播放时钟、下载/缓存、AI 服务、漫画排版与翻译策略。
本方案不把所有应用第三方依赖都转移到 SDK，也不以更换 CPAL/WGPU 为前置条件。
小型的像素访问、矩形校验、增益、WAV 头和缓冲复制可由 remdia 自己实现，无需为这些操作再引入大型库。

## 当前需要迁移的路径

- native 插件：Symphonia 解复用/音频解码、libopus、rubato、LAME、image 缩放。
- macOS 视频插件：VideoToolbox 已是系统解码；另有 AVC/HEVC 参数解析库。
- Windows 视频插件：已使用 Media Foundation；音频插件目前仅部分 AAC/Dolby 路径走系统。
- replayer：漫画预览 decode/resize/tile；封面 decode/PNG 保存；视频缩略图保存；译图 JPEG 编码、白底合成、裁剪、回填；品牌图片缩放；相关测试的图片生成和导出。
- replayer 译图还有白边扫描、带重叠的分段与模型请求：这些业务策略留在应用，通过 remdia 的像素视图和图像操作实现。
- 应用当前存在额外的图片 codec dev profile 优化，迁移后随直接依赖一并清理。

## 平台组件映射

| 能力 | Windows 优先实现 | macOS 优先实现 | 缺口处理 |
|---|---|---|---|
| 图片探测/解码/编码/metadata | WIC | ImageIO | 按读、写、格式、帧类型分别探测 |
| 图片缩放/旋转/颜色转换 | WIC bitmap transform / color transform | ImageIO 缩略解码、CoreGraphics、Accelerate/vImage | CPU 路径先行，适配明确的像素/alpha 契约 |
| 裁剪、拼接、铺底、tiles | 平台 bitmap 操作与自研有界像素操作 | CoreGraphics 与自研有界像素操作 | 不引入新图像算法库 |
| 容器读取/压缩包输出 | MF Source Reader / Media Source | AVFoundation AVAssetReader | 系统不能读取的容器才走 demux fallback |
| 音频解码 | 独立音频 MFT | AudioConverter | 格式、profile、声道布局实测后决定 |
| 重采样/声道转换 | Audio Resampler DSP | AudioConverter | drain、延迟、矩阵和样本数需单独验收 |
| 视频解码 | 当前 MFT 组件 | 当前 VideoToolbox 组件 | 本轮先保持已验证的 CPU NV12 路径 |
| 音频编码 | 系统 encoder MFT | AudioConverter | 编码与解码分开判断，特别是 MP3 |
| 容器写入 | 系统 sink（适合组件接口的部分） | AudioFile / AVAssetWriter（适合组件接口的部分） | WAV 等简单封装可保留自研实现 |

Windows 的 [WIC codec 清单](https://learn.microsoft.com/en-us/windows/win32/wic/native-wic-codecs)同时列出内置 codec 和商店扩展，WebP 属于扩展路径；不能保证干净系统具备它。
Apple 建议分别用 [ImageIO source/destination 类型枚举](https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/ImageIOGuide/imageio_basics/ikpg_basics.html)判断读、写能力。
Windows 有独立的 [MP3 encoder MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/mp3-audio-encoder)，不能根据旧媒体格式总表判定缺失；其输入约束需要在图中插入 PCM 格式转换。
Windows 的 [Audio Resampler DSP](https://learn.microsoft.com/en-us/windows/win32/medfound/audioresampler)可作为替换 rubato 的首选候选。

### 能力调查的现有证据

本机 macOS 27 arm64 通过 ImageIO 与 AudioFormatGetProperty 的只读枚举：

- ImageIO source：包括 JPEG、PNG、WebP；destination：包括 JPEG、PNG，未列出 WebP。
- AudioConverter decoder：包括 MP3、AAC、FLAC、ALAC、Opus、Vorbis、AC-3/E-AC-3；encoder 未列出 MP3。
- 原始结果保存在本地 `target/system-media-design/macos-capabilities.json`（不纳入发布包）。

这只是枚举，不是逐格式/逐 profile 解码认证，也不能外推到 macOS 15 或 Windows。
AudioConverter 的 [EncodeFormatIDs/DecodeFormatIDs](https://developer.apple.com/documentation/audiotoolbox/kaudioformatproperty_encodeformatids)是能力调查入口，之后仍需创建转换器并跑样本。
Windows 11 24H2 的干净安装[不再默认包含 AC-3 codec](https://support.microsoft.com/en-us/windows/apps/windowsmediaplayer/troubleshoot-windows-media-player-errors)，OEM/升级安装可能不同；Dolby、HEVC、AV1 等均须单独测试可用性。

## 能力发现与选择

新增 `Engine::capabilities()`、`Engine::explain_plan()` 及开发 CLI `remdia capabilities --json`。
能力记录包含 operation、format/codec、profile、输入输出像素/PCM 格式、声道布局、读/写方向、可选扩展、provider 及限制。
区分系统内置、系统扩展、其他已安装 provider、项目携带 fallback；通过系统注册表/API 被发现不等于系统自带。
Windows 优先已认证的系统 provider，未认证的注册 codec 不自动取得与系统内置相同的优先级。

发现、实例化和样本认证是三个层级：manifest 中声明了工厂，不代表当前机器能成功处理具体输入。
能力查询使用新增的控制组件/现有 command 协议返回有界 JSON，避免为了枚举引入平台对象到公共 ABI。
查询结果按 OS build/provider 版本记录，支持刷新；初始化失败或扩展安装变化后不能永久复用旧缓存。

选择粒度为图节点，不是“整段媒体换引擎”：

1. 系统组件满足当前格式与参数，优先选择。
2. 系统返回明确的能力缺失/不支持，且此能力在发布配置的 fallback 白名单中，才选择对应窄功能组件。
3. 输入损坏、取消、预算不足、I/O 失败不触发换库重试；模糊的系统错误不能一概解释为“不支持”。
4. 系统实现的已知兼容性问题需要具体 OS/provider/输入范围和回归样本，才能成为例外规则。

提供 `SystemOnly` 与 `SystemPreferred` 两种策略；后者仅在已配置并打包的白名单内回退，不动态下载库或自动安装商店扩展。
replayer 发布配置使用 SystemPreferred，CI 强制另跑 SystemOnly，防止 fallback 掩盖系统实现缺陷。
开发日志/计划结果记录各节点 provider、选择原因及降级点；普通用户只需看到可理解的格式或扩展缺失提示。

## demux 仍与 decoder 分开

Windows Source Reader 请求 GetNativeMediaType 返回的原生类型来[读取未经解码的数据](https://learn.microsoft.com/en-us/windows/win32/medfound/processing-media-data-with-the-source-reader)。
macOS AVAssetReaderTrackOutput 的 [outputSettings=nil](https://developer.apple.com/documentation/avfoundation/avassetreadertrackoutput/outputsettings)用于取原格式样本，并保持 decode order。
适配层负责转换成统一 packet：PTS/DTS、duration、time base、keyframe、codec initialization、priming/trim、流 ID。
不得把“系统能播放此文件”直接等同于“系统能提供符合组件契约的压缩 packet”。

MP4/MOV 等优先认证系统 demux；MKV/Ogg/特殊容器是待确认的 fallback 候选，不先声称所有平台都支持或都不支持。
例：`fallback.demux → system.audio.decode → system.audio.resample` 完全合法，不能因为 demux 回退就顺带切换成第三方 decoder。
元数据、封面、Range 读取、seek 与 EOF 后重播同属 demux 验收范围。
现有 HTTP 缓存/下载语义通过 IMFByteStream / AVAssetResourceLoader 等系统 source 适配验证；分清本地完整文件、普通 HTTP Range 和尚未完整下载的文件。
后两者必须保持超时、中断与 generation；不偷偷先下载整个文件。若某系统 source 不适用，只对该 source/demux 能力回退。

## 统一媒体图中的图片组件

公共执行入口沿用 Engine 的 plan/open/graph 模式。扩展统一请求类型以表达音频、视频和图片任务，保留现有音视频入口作为兼容包装。
图片模块仅组织图片描述/配置类型，不建立独立运行时；不暴露 DynamicImage、RgbaImage、CGImageRef 或 IWICBitmap。
ImageInfo 包含存储尺寸、应用 orientation 后的显示尺寸、format、frame count、orientation、色彩信息与 alpha 模式。
图片像素实际存放在同一 MediaBuffer 中，FrameInfo::Image 描述它；ImageBuffer 如保留此名称，仅是已验证的类型视图，不建立第二份存储或所有权机制。
类型视图提供尺寸、stride、像素格式和只读 bytes/row 视图。
明确支持 RGBA8/BGRA8、sRGB、straight/premultiplied alpha；第一轮仍以 SDR 为认证目标，高位深/HDR 不静默当作 8-bit sRGB。
缺失色彩描述使用明确的默认策略，存在 ICC/profile 时由系统转换；orientation 只应用一次。

统一入口的拟定形态（仅示意，未实现，命名待实现时定稿）：

```rust
let request = MediaRequest::Image(ImageRequest { /* 输入、输出、变换、预算 */ });
let plan = engine.plan_job(&request)?;           // 与音视频共用 planner/registry
let mut session = engine.open_job(request, cancel)?; // 执行同一个 Graph
// 高级调用方可显式组合相同的组件：
let mut graph = unsafe { engine.graph(&plan.graph, cancel)? };
```

上面的 open_job 与显式 graph 是两种可选调用方式，不会为一个任务同时运行两份图。
probe/decode/thumbnail/transform/tiles/compose/encode 是可组图的任务/操作配置，不再作为一组独立图片服务方法。
source 首版支持路径、共享内存字节；远程图片仍由 replayer 下载后提交，不重复其鉴权/HTTP 缓存。
transform 覆盖 fit/fill/exact resize、crop、rotate/flip、像素转换、alpha 转换、background 合成。
encode 明确指定 JPEG/PNG、质量、输出字节上限、metadata 保留策略。JPEG 白底合成和 replayer 的 quality=95 行为保持；不同系统 encoder 的质量数值不承诺字节等价。
派生/上传图片默认剥离非必要 metadata，方向归一化后同步更新/移除原 orientation，避免重新读取时再次旋转。
GIF/多页图的 frame_count/动画属性可探测，第一轮默认显式选择第一帧；不将动画播放、SVG/PDF 光栅化自动列入首版。

底层工厂按职责包括 image.decode、image.scale、image.crop、image.orient、image.color、image.alpha、image.tile、image.compose、image.encode；planner 按能力选择 windows.* / macos.* / fallback.* 的具体 ID。
系统 API 如能安全融合部分步骤，可以作为具有明确能力声明的优化组件，由 planner 选择，不能让通用入口依赖隐藏的整段图片处理实现。

| 媒体任务 | 同一个 Graph 中的组合示例 |
|---|---|
| 音频导出 | source → demux → audio.decode → audio.resample → audio.encode → mux → sink |
| 视频缩略图 | source → demux → video.decode → frame.select → pixel.convert → image.scale → image.encode → sink |
| 图片预览 | source → image.decode → image.orient → image.scale → image.tile → output |
| 图片转换 | source → image.decode → image.crop → image.alpha → image.encode → sink |
| 音乐封面 | demux/artwork.extract → image.decode → image.scale → output / image.encode |

图只包含任务需要的节点，静态图片无需为了外观一致而增加无意义的 demux/mux。
跨视频帧/图片像素的过滤器复用实现，并显式声明兼容的端口类型；需要时用有明确类型和时间语义的适配节点连接，不能靠 reinterpret 偷换类型。
同一解码图片可扇出到预览、编码、tiles 等多个分支，共享不可变 MediaBuffer，统一接受边队列背压。
拼接首版是固定 canvas 加有序 patch 的单个图节点，遵循同样的 submit/receive/drain 协议，逐个提交并在 drain 后冻结结果；不为图片迁移额外扩张当前单输入端口图模型。
AI 分段与 overlap 位置由 replayer 决定，remdia 只执行明确的 rect/patch 指令。

### 像素、线程和内存

- 缩放时规定 alpha/色彩计算语义；透明边缘不能变黑，品牌图标现有预乘 alpha 行为单独认证，不能直接套用任意系统默认缩放。
- COM/WIC、ImageIO/AudioConverter 活对象留在创建它们的 worker；输出仅传可安全跨线程销毁的 CPU 缓冲，release 回调满足现有 ABI 约束。
- 图像对象 retain/release、codec 内部缓存、临时缩放/ICC 缓冲均纳入资源分析。系统 API 的不可见内部分配不能宣称受到 SDK ledger 的硬限制。
- DecodeOptions 在创建完整位图前检查输入字节数、宽高、像素数、帧数、输出尺寸与计算溢出。不得先解码巨图再缩到预览尺寸。
- 优先系统缩略解码或按需求输出，仍需验证 codec 是否内部整图解码；不承诺所有格式的任意 ROI 都能降低峰值内存。
- 保持当前预览上限 16M 输出像素、tile 高度 2048 的使用体验，并保留可配置预算；tile 使用共享只读底图视图或按 tile 输出，避免分块时再复制完整底图。
- 译图 canvas 和 patches 纳入峰值预算；首版超过预算明确失败，不为了成功偷偷创建数 GiB 中间位图。
- OS 同步调用不能总被立即取消，优先缩小调用块并在调用前后检查 token；过期 generation 结果不上传 UI。
- 缓存由应用管理；key 加入 operation、像素/alpha/色彩策略和实现版本。原有 PNG/JPEG 译图缓存继续可读；WebP 缓存仅在系统具备对应 codec 时可读。系统不支持 WebP 时直接报错，不自动触发付费重译。

## API/ABI 演进

建议 SDK 版本升级至 0.3，扩展 JSON schema 与 ABI minor 至 2.2，保持主版本 2 与现有 C 结构布局。
新增 `DataKind::Image`、`FrameInfo::Image`，并明确其 metadata、plane 长度、stride、rect 与时间/帧语义。
新增 Image variant 不是对老宿主透明兼容：2.1 宿主不得加载/协商需要 Image schema 的工厂。
新宿主可加载 2.1 音视频工厂；能力查询缺失时保守使用旧 descriptor，不能推断新功能存在。
实现时同步审查 query 的版本比较、manifest 过滤和 struct_size；用旧/新宿主与插件交叉矩阵证明兼容，不只修改版本号。
如实现最终需要改变 C 函数签名或结构语义，则另定主版本升级，不能借 minor 强行兼容。

## 插件和第三方依赖拆分

核心与 Windows/macOS 系统 pack 不依赖 Symphonia、libopus、rubato、LAME、image。
原 native pack 拆为公共的小型基础组件和按缺口构建的 fallback pack，避免一个 MP3 编码需求携带整套软件 decoder。

| 现有库 | 迁移决策 |
|---|---|
| image / fast_image_resize | replayer 与系统 pack 移除；系统图片路径完成后 CLI 也用 remdia 编码 |
| rubato | 两平台系统重采样认证通过后移除，不保留默认双实现 |
| Symphonia | 先保留按 feature 精简的 demux 候选；codec features 只为已证明的 decoder 缺口开启 |
| libopus | 先测两平台系统 Opus；本机已枚举到，不能继续无条件打包 |
| LAME | Windows 优先系统 MP3 encoder；macOS 当前枚举缺失，作为 macOS MP3 编码的窄 fallback 候选 |
| h264-reader / scuffle-h265 | 优先系统 format description/重排信息；如果现有 reorder 正确性仍需头解析，保留小型 parser，待证明可替代再移除 |
| WebP | 仅使用系统 codec，缺失时报不支持；不携带第三方解码器 |
| Vorbis 等补充实现 | 仅在必需用例、最低 OS/干净安装矩阵证明缺失后选最小实现，不默认恢复大型通用 audio 栈 |

独立 fallback 具有独立 manifest、平台构建开关、许可证清单和可审计依赖；未选用的平台/能力不编译也不打包。
提供 system-only 认证配置和 replayer 的明确 compatibility 发布配置，优先级不能替代物理拆包。
系统API绑定和通用 Rust 基础库仍可使用；本轮约束针对媒体/图像算法实现，不要求重写 serde 等基础设施。

## replayer 迁移

1. 用统一 remdia 请求、FrameInfo::Image 与 MediaBuffer 类型视图替换 Artwork、漫画 Bitmap 的外部图像类型，UI 只消费宽高、stride 与像素视图。
2. 漫画缓存/加载提交预览/分块图；线程池、并发合并、缓存淘汰、磁盘原子写仍在应用。
3. 翻译 provider 改用 remdia ImageBuffer/encoded bytes；crop/resize/paste/JPEG 白底编码走 SDK，分段策略与 AI 请求留在应用。
4. 封面、视频缩略图、音乐下载侧边 PNG、品牌图标、测试图片生成统一迁移。品牌图标在首次 UI 绘制前异步准备或预生成小尺寸原始像素，避免依赖 UI 线程上的 plugin 初始化。
5. 删除 image、fast_image_resize、replayer-image 和相关 dev profile/旧许可打包，更新两端构建/安装脚本与 pinned revision。
6. 保持旧译图缓存、音视频 seek、device recovery、字幕 mono 16kHz PCM、MP3 导出、便携包插件发现行为。

直接依赖移除与整个 Cargo 图消失是两个检查：eframe/图标/平台层可能间接引入图片 crate，需要 `cargo tree -i` 找到来源。
仅调整可控的非必要 feature；GUI 平台内部实现若仍使用该库，应如实列明，不能宣称整个进程零第三方图像代码。

## 分阶段交付与验收

### P0：能力与基线

两平台枚举并用实际样本验证 decode/encode/container/filter，产出平台/OS/扩展矩阵。
测当前提交的构建时间、RSS/footprint、包体、启动、漫画 decode/resize/encode、音频首包/seek 与视频缩略图。
固定样本、尺寸、质量策略、线程/并发数、编译器与 release 配置，分别记录冷/热缓存。

### P1：图片 SDK 和 replayer 图片迁移

完成图像模型、WIC/ImageIO pack、JPEG/PNG/WebP 读取与 JPEG/PNG 输出、变换与 tiles、预算、CLI 和 C ABI 测试。
WebP 不可用的平台返回不支持；按更新后的用户要求移除第三方 WebP fallback，旧 WebP 缓存依赖已安装的系统 codec。
迁移全部 replayer 图片调用，并删除应用自己的图片 codec 库。先让用户看到可验证的依赖收敛。

### P2：音频系统化

所有已支持音频格式、重采样、声道矩阵、编码进入能力规划；测 gapless、priming/trim、drain、seek generation、混音与字幕样本数。
同一输入不同后端不要求 PCM 每位相同，但时长、声道顺序、trim、脉冲位置和误差需达到固定标准。
系统 MP3 encoder 对 Xing/延迟/末尾 padding 不保证与 LAME 一致，须验证重开时长与尾部，不能只检查文件非空。

### P3：系统 demux 与最小 fallback

先迁移已验证的容器，复验本地、HTTP Range、渐进下载、元数据/封面、重复 seek/EOF/replay。
在能力矩阵基础上删减 Symphonia codec features、Opus、LAME 和 parser 依赖，形成平台独立 package manifest。
图片/音频/容器分别上线，不等待一次性重写所有媒体路径；每阶段都有真实可运行的 replayer 包。

### 最终验收

- Windows 干净 VM、含/不含可选 WebP/媒体扩展环境，macOS 15 与当前版本，ARM/Intel 包。
- 架构：图片请求入口和显式 GraphSpec 生成相同的节点/输出；验证替换单个 provider、扇出共享缓冲、背压、取消和预算；检查图片接口不存在直接绕过 Graph 调用系统处理的路径。
- 图片：EXIF 方向、ICC、透明边缘、RGB/灰度、超长图、损坏图、超预算、取消/并发，JPEG/PNG 输出与旧 WebP 缓存；不同系统 codec 用解码后指标和视觉样本验收，不要求编码文件 SHA 一致。
- 译图：旧缓存读取、白底 JPEG、overlap 裁剪回填、保持画布尺寸；测试用本地固定样本，不能为了回归调用付费模型。
- 音视频：现有测试全过，补充系统-only 与 fallback 混合图、重采样首尾、声道矩阵、B 帧与重排序。
- 性能：逐阶段报告 P50/P95、峰值 RSS/footprint、并发时内存、冷/热构建与包体；不预先保证系统实现一定更快或更省内存。
- 包体/依赖：便携安装后移除开发 PATH，导入表/框架审计、fallback 单独清单、`cargo tree` 验证应用直接图片库已移除。

推荐实施顺序：P0 → P1（含 replayer 图片迁移）→ P2 → P3。保留现有已验证的视频解码能力，先完成用户实际用到的统一图片入口，再收敛音频和容器依赖。
