# replayer integration

Windows replayer now uses remdia's public component graph for playback, metadata/cover extraction, thumbnails, independent subtitle PCM, MP3 encoding and downloaded-audio validation. Its Windows target has no ffmpeg-next dependency. The existing CPAL device callback, playback clock and WGPU conversion shader are retained.

| Host subsystem | remdia path |
| --- | --- |
| Playback worker | native.demux → native.audio.decode → native.audio.resample; native.demux → windows.video.decode |
| Frame presentation | Read-only remdia frame planes → WGPU native NV12, with a CPU fallback |
| Probe / cover | native.demux metadata and artwork command; no decoder created |
| Thumbnail | native.demux → windows.video.decode → native.video.rgba |
| Subtitle extraction | Independent mono 16kHz Audio graph; range seek and bounded chunks |
| MP3 requests | Standalone native.mp3.encode factory; final delay/padding tag patch |
| QQ / torrent / online music | Application prepares a local file or loopback HTTP URL; remdia handles bounded Range reads |

A media graph stays on its owning worker. The subtitle scheduler transfers an input/configuration ticket and creates a new graph on the decode thread, rather than moving live component handles. UI cancellation/seek interruption uses a shared token. The host owns pause, volume, clocks, output-device recovery and generation filtering; the backend never opens an audio device or renders a window.

The replayer Windows package must include both native/windows DLLs and manifests under plugins, using a pinned remdia revision. Existing macOS releases retain their old backend until a native video pack is implemented there; remdia itself has no FFmpeg dependencies on any target. The remdia-media feature enables the portable integration path for audio/host validation on other development machines.

The initial Windows certification target is H.264 MP4/MKV plus common music formats. System-codec absence, unsupported video codecs, hardware acceleration and unusual container variants remain explicit limitations, not silent fallback to FFmpeg. Full historical format parity with replayer's old FFmpeg build is not claimed.
