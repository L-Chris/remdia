# Third-party software

remdia's original core, graph runtime, CLI and adapter code are MIT licensed.
Builds, packaged runtimes and test execution have no FFmpeg dependency.
Exact source crate versions and archive checksums are pinned in Cargo.lock.

The foundation (`native`) and system audio/image/demux packs contain no bundled
media decoder, resampler or image codec implementation. Platform frameworks
are supplied by the operating system and are not redistributed.

Compatibility components use the following sources. The bundled SDK links its
selected system/utility components together; macOS LAME remains an independent
replaceable library. Static registration changes neither source provenance nor
the required license notices. See `docs/linking.md` for distribution layouts.

- **SpeexDSP 1.2.1 standalone resampler (BSD-3-Clause)**,
  https://github.com/xiph/speexdsp/tree/1b28a0f61bc31162979e1f26f3981fc3637095c8 :
  Android's optional `fallback_resample` includes only the unmodified standalone
  SRC and two required headers, with private symbol names. It is statically
  registered into the unified Android SDK when enabled. No Speex audio codec,
  echo canceller or jitter buffer is built. The complete license and exact source
  provenance are in `plugins/fallback-resample/vendor/`. The Android AAR includes
  the license; the `--system-only` AAR omits this plugin. Windows/macOS packages
  retain their existing system SRC and do not ship this compatibility pack.
- **Symphonia 0.6.1 (MPL-2.0)**, https://github.com/pdeljanov/Symphonia :
  `fallback_demux` registers format/metadata readers only, for unsupported
  source/container profiles including the existing HTTP/Range adapter. ADTS,
  MPA and FLAC readers share upstream crates with decoder code, so those crates
  remain in Cargo.lock. MPA's decoder features are disabled; AAC/FLAC still
  compile their upstream decoder modules. No audio decoder is registered or
  created in this pack.
  Vorbis software decoding is not included; unavailable system codecs are unsupported.
- **mp3lame-encoder 0.2.5 / mp3lame-sys 0.1.11 (LGPL-3.0), LAME 3.100**,
  https://github.com/DoumanAsh/mp3lame-encoder and
  https://github.com/DoumanAsh/mp3lame-sys : `fallback_mp3_lame` supplies the
  certified 16-kHz gapless encoding contract on **macOS only**, where
  AudioToolbox does not expose a system MP3 encoder. The macOS separately
  replaceable fallback dylib statically incorporates the wrapper and matching
  LAME sources. Retain their LGPL notices and source/relink provisions when
  distributing macOS builds. Rebuild this public revision with Cargo.lock;
  replace the fallback component, rather than relinking the MIT host/core.
  Windows uses Media Foundation encoding and remdia's independent gapless mux;
  it does not build or distribute LAME. A LAME-compatible metadata tag is a file
  format marker, not an encoder implementation; ID3 TENC identifies the actual
  encoder as Windows Media Foundation / remdia.
- **h264-reader 0.9 (MIT OR Apache-2.0)**,
  https://github.com/dholroyd/h264-reader , and **scuffle-h265 0.2.2
  (MIT OR Apache-2.0)**, https://github.com/scufflecloud/scuffle : macOS video
  parameter-set parsing and conservative decoded-picture reorder bounds. These
  are header parsers; VideoToolbox performs video decoding.

Rubato, libopus and the libopus adapter have been removed. Windows/macOS audio
resampling and all supported platforms' Opus decoding use system APIs.
Android media and images use public framework APIs through a JNI bridge;
JNI/libc crates provide bindings, not codec implementations.
Apple AudioToolbox, AVFoundation, VideoToolbox,
ImageIO/CoreGraphics and Windows Media Foundation/WIC retain their platform
terms; installed optional codecs may have additional terms.

Other dependencies include anyhow/serde/serde_json/clap/reqwest (MIT OR
Apache-2.0), libloading (ISC), TLS libraries and their transitive dependencies.
They are infrastructure/bindings rather than bundled media implementations.
License texts for redistributed media components accompany portable packages
under `licenses/`; retain the notices when redistributing.

Synthetic tone/video/image fixtures are project-owned and MIT licensed.
Generation history is documented with the fixtures; ordinary tests read the
committed bytes. `docs/remedia.md` is a historical architecture proposal and
mentions other backends only as design context.
