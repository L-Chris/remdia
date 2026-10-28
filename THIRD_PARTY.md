# Third-party software

remdia's original SDK, graph runtime and CLI code are MIT licensed. There is no FFmpeg dependency in the Cargo graph, runtime, package or test toolchain.

Native capability packs incorporate these separately licensed libraries:

- Symphonia 0.6.1 (MPL-2.0): https://github.com/pdeljanov/Symphonia . Its Rust demuxers and audio decoders are compiled without modifications; exact source crates/checksums are recorded in Cargo.lock.
- symphonia-adapter-libopus 0.3 (MIT OR Apache-2.0), with vendored libopus (BSD-3-Clause): https://github.com/aschey/symphonia-adapters and https://opus-codec.org/ . opusic-sys builds its matching vendored source using CMake.
- rubato 0.16.2 (MIT): https://github.com/HEnquist/rubato . Stateful sinc resampling.
- mp3lame-encoder 0.2.5 and mp3lame-sys 0.1.11 (LGPL-3.0): https://github.com/DoumanAsh/mp3lame-encoder and https://github.com/DoumanAsh/mp3lame-sys . The latter includes matching LAME 3.100 sources. The native capability DLL statically incorporates LAME and the LGPL wrapper; its distribution must comply with their LGPL requirements, including source and the ability to rebuild/relink a modified library. The independently replaceable native DLL can be rebuilt from this public source and the Cargo.lock-pinned source distributions; the host/core remains MIT.
- h264-reader 0.9 (MIT OR Apache-2.0): https://github.com/dholroyd/h264-reader . AVC parameter parsing and conservative decoded-picture reorder limits.
- scuffle-h265 0.2.2 (MIT OR Apache-2.0): https://github.com/scufflecloud/scuffle . HEVC parameter parsing. Both are Rust header parsers, not linked video decoder implementations. License texts are included in licenses.
- Apple VideoToolbox, CoreMedia, CoreVideo, CoreFoundation and AudioToolbox are macOS system frameworks. The native pack loads AudioToolbox's public AudioConverter API for AAC/AC-3/E-AC-3 through libloading; the macOS video pack links the other frameworks. No Apple framework binaries are redistributed.
- Windows Media Foundation is a Windows system facility; the native pack also uses its AAC and Dolby MFTs. No independent AAC/Dolby decoder fallback or system codec binaries are bundled. Additional Windows codecs/extensions retain their own terms.

Other Rust dependencies include anyhow/serde/serde_json/clap/image (MIT OR Apache-2.0), reqwest (MIT OR Apache-2.0), libloading (ISC), their TLS dependencies, and their transitive dependencies. Exact versions and source archive checksums are in Cargo.lock. License texts for the codec distributions are included under licenses in portable packages. Retain these notices when redistributing.

The tiny synthetic video/tone fixtures were generated for this project and are provided under the project's MIT license. They require no external media program to run tests. The original architecture proposal under docs/remedia.md discusses FFmpeg only as a comparison baseline and historical design context.
