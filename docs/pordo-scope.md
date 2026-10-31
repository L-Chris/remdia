# Pordo integration

Windows and macOS use one public remdia component graph for playback, images,
metadata/artwork, thumbnails, subtitle PCM and MP3 requests. Pordo retains
CPAL output, WGPU rendering, clocks, caching, network source policy and AI work.

| Host subsystem | remdia path |
| --- | --- |
| Playback | System demux or source/container compatibility reader → independent system audio/video decoder → system SRC |
| Rendering | Read-only CPU NV12 planes → application's existing WGPU renderer |
| Metadata/artwork | Demux metadata/command; no decoder is created |
| Thumbnail | Video decoder → scalar RGBA conversion → image.from_video → system image.scale |
| Comics/covers/translations | Shared buffer source → image decoder/filter/tile/compose/encoder components |
| Subtitle extraction | Independent mono 16-kHz graph, range seek, bounded PCM chunks |
| Gapless MP3 requests | Shared input source → narrow encoder fallback → final delay/padding tag patch |
| Online music/torrent/QQ | Application prepares a local file or loopback HTTP source; compatible source/demux preserves interruptible Range and progressive reads, followed by system decode |

The application directly imports remdia Image/MediaBuffer rather than image,
fast_image_resize or a separate application image crate. GUI framework clipboard and
icon internals still depend on image transitively; this is not a zero-image-crate
claim for the application's full dependency graph.

Graphs and components stay on their creating worker. Frames and cancellation
controls cross threads; subtitle workers open their own graph from an input
configuration ticket. Pause, device recovery, clocks and generation filtering
remain application responsibilities.

Package the exact pinned SDK revision, all platform manifests/libraries and its
notices. macOS uses native, macos, image, system_audio, system_demux,
fallback_demux and fallback_mp3_lame. Windows uses native, windows, image,
system_audio, system_demux, fallback_demux, fallback_mp3_lame and fallback_vorbis. Each compatibility component is selected only for its declared
source/format/profile gap. System failures do not trigger generic codec retries.

Certification targets Windows x64 and macOS 15+ arm64/x86_64, common music and
SDR H.264 MP4/MKV, PNG image caches and WebP caches where a system codec is installed, seek/reset/EOF, progressive reads
and relocated packages. Historical FFmpeg format parity, HDR, shared GPU
surfaces and every legacy audio profile are not claimed.
