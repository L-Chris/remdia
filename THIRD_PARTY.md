# Third-party software

remdia's original source is MIT licensed. Runtime libraries retain their own licenses. MP3 dynamic-buffer muxing follows the implementation in L-Chris/replayer's `src/subtitles/mp3.rs`; the two projects share the same maintainer.

The FFmpeg capability pack uses ffmpeg-next 9 (WTFPL), ffmpeg-sys-next (WTFPL), FFmpeg 9.0.2 and the libmp3lame encoder. FFmpeg's LGPL/GPL status depends on its build configuration. The Windows bootstrap uses the existing LGPL shared development build distributed at:

- Binary and development files: https://github.com/L-Chris/replayer/releases/tag/deps-ffmpeg-9.0.2
- Upstream build system and corresponding dependency source retrieval: https://github.com/BtbN/FFmpeg-Builds
- FFmpeg source and legal information: https://ffmpeg.org/download.html and https://ffmpeg.org/legal.html
- LAME source: https://lame.sourceforge.io/

The bootstrap archive SHA-256 is pinned in scripts/setup-windows.ps1. Keep its bundled license, build configuration and dependency notices alongside redistributed DLLs; the packaging script copies those files from the archive. Shared codec DLLs remain replaceable and are not statically linked into the host. Replacing the bootstrap with another build requires reviewing that build's notices and source obligations. The bootstrap is a compatibility starting point, not a size-optimized runtime.

Rust dependency versions are recorded in Cargo.lock. Dependencies include anyhow (MIT OR Apache-2.0), serde/serde_json (MIT OR Apache-2.0), libloading (ISC), clap (MIT OR Apache-2.0) and image (MIT OR Apache-2.0), with their transitive dependencies. Consult each crate's source distribution for its complete license text before redistributing a customized package.
