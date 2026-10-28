# replayer 首版范围与接入

参考源是本工作区 `replayer` 的 `src/media.rs`、`src/player/{demux,video,audio}.rs`、`src/subtitles/{audio,mp3}.rs`。此次新建独立仓库，不改动原应用及其未提交改动。

| replayer 位置 | remdia 接口 | 应用仍负责 |
| --- | --- | --- |
| media::probe_info / info | Probe session、MediaInfo、artwork | sidecar 标签、图片解码与本地库缓存 |
| media::video_thumbnail | Thumbnail session、seek、RGBA planes | 图片展示与缩略图采样策略 |
| player::demux / video | Playback session、read、seek、generation | 命令队列、旧帧丢弃、视频调度 |
| player::audio | Audio frame、可配置采样率、mono/stereo f32le | CPAL 输出、音频时钟、音量、设备恢复 |
| subtitles::AudioChunks | 独立 Audio session、audio_chunk | AI 任务调度、静音切分、重试 |
| subtitles::mp3 / Chunk::wav | encode_mp3 / AudioChunk::wav | HTTP 请求与字幕文本处理 |
| media_source / torrent | 将准备后的路径或 loopback URL 传入 source | QQ 解密、请求凭据、下载缓存、torrent 读门控 |

集成顺序：先把字幕独立输入替换为 `Task::Audio`；再替换探测与缩略图；最后在媒体 worker 中替换解码部分。保留 replayer 已验证的 CPAL/WGPU/clock 代码。Native Video metadata 中包含 pixel format、各 plane stride、SAR 和色彩字段；音频 `bytes()` 是 packed little-endian f32，不要假定其地址满足 f32 对齐，使用字节转换或无对齐读取。

应用持有取消 token，通过现有后台任务创建和关闭 Session。seek 返回新的 generation，视频队列与音频 ring buffer 都应按 generation 失效。read() 为拉取接口，宿主在自己的有界队列有空位时再调用；不要在 UI 主线程调用。

v0.1 不是直接二进制替换 `ffmpeg-next` 的兼容层；迁移需要适配上述接口，并重新执行 replayer 的 seek、短音轨、无声视频、网络阻塞、设备丢失、字幕范围等测试。此次仓库的集成语料验证了 SDK 行为，不能代替完整应用验收。
