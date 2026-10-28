# Third-party software

remdia's original core, graph runtime, CLI and adapter code are MIT licensed.
Builds, packaged runtimes and test execution have no FFmpeg dependency.
Exact source crate versions and archive checksums are pinned in Cargo.lock.

The foundation (`native`) and system audio/image/demux packs contain no bundled
media decoder, resampler or image codec implementation. Platform frameworks
are supplied by the operating system and are not redistributed.

Separately replaceable compatibility packs use:

- **Symphonia 0.6.1 (MPL-2.0)**, https://github.com/pdeljanov/Symphonia :
  `fallback_demux` registers format/metadata readers only, for unsupported
  source/container profiles including the existing HTTP/Range adapter. ADTS,
  MPA and FLAC readers share upstream crates with decoder code, so those crates
  remain in Cargo.lock; no audio decoder is registered or created in this pack.
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

Rubato, libopus and the libopus adapter have been removed. Audio resampling and
Opus decoding use the system APIs. Apple AudioToolbox, AVFoundation, VideoToolbox,
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
