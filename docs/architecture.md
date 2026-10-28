# Component graph architecture

The project has no FFmpeg build, runtime, command-line or codec dependency. The old fused session backend has been removed.

```text
remdia-cli / replayer -> remdia playback facade / public Engine::graph
                      -> remdia-graph: typed buffers, ports, DAG scheduler
                      -> remdia-buffer: shared resource ledger
                      -> remdia-plugin-abi: versioned platform C ABI v2
                                  ^
        native pack: Rust demux, audio decoder, filters, encoder, mux, sink
        Windows pack: independent Media Foundation video decoder
        macOS pack: independent VideoToolbox video decoder
```

A capability pack registers multiple independent factories. Components are created individually, and every graph node owns its own submit/receive/drain/reset state. Media samples cross DLLs as immutable byte views with explicit retain/release callbacks. Native code remains loaded for the process lifetime. A session is a convenience graph builder; it does not contain a second, fused implementation.

## Graph execution

A JSON GraphSpec contains named nodes, component IDs/configuration, and typed edges. Edges can select a stream ID and independently limit bytes, buffer count and queued media duration. Construction rejects missing nodes, duplicate names/edges, cycles, incompatible port constraints and unconnected inputs. Ports declare packet, audio, video or container bytes; optional formats/rates/channels refine compatibility. The current memory domain is CPU; GPU resources are not silently treated as CPU buffers.

The runtime visits nodes in reverse topological order. A node's pending output is routed only when every matching outgoing edge can accept it. Fanout shares immutable buffers. Downstream receives outputs before submitting another input. WOULD_BLOCK leaves an edge's input untouched. Producers reach EOF first; each consumer receives drain only after all incoming edges are empty and their producers ended. Mux finalization therefore follows decoder/filter tails, and sink publication follows mux patches. The runtime has no per-node thread pool.

Before receive, the ledger reserves the advertised maximum output allocation; ABI 2.1 RECONFIGURED refreshes negotiated bounds and revalidates edges before the next output call; successful output reduces that reservation to the plane bytes actually returned. Clones share their owner. Cross-DLL wrappers can retain earlier reservations, so some pass-through paths are conservatively overcounted. Codec-internal allocations, source buffers, system MFT allocations and temporary file/export buffers are not a hard process memory cap. If reservation fails before a call, the input stream is not advanced.

Arbitrary composition means compatible contracts, not arbitrary media conversions. A PCM edge cannot connect to a compressed-packet decoder. WAV requires its configured rate/channels and the planner/caller inserts a resampler. Multiple outgoing branches can apply different gains, sample rates and muxers. A demux → packet.copy → mp3.mux → sink path never creates a decoder. Output filenames must not exist; a sink uses a private temporary file and hard-link publication to prevent overwriting another writer.

## Time and cancellation

Packets retain PTS/DTS, duration, time base, trim start/end and stream identity. Symphonia supplies signed composition timestamps and gapless packet trims. Audio decoder output starts after packet trim; zero-length trimmed buffers do not establish a resampling anchor. Resampling uses a bounded, stateful sinc filter, trims its startup delay, and flushes exactly the admitted sample count at EOF. A 120-second cap bounds subtitle PCM chunks.

Seek records known random-access points and, for H.264/HEVC, searches preceding compressed packets with a bounded exponential pre-roll window when needed. A keyframe gate prevents starting a decoder on a preceding B/P frame. A fresh reader restores MP4 iterator state after EOF or interrupted partial reads; unknown random-access formats safely pre-roll from the start. All downstream nodes and queues reset under a new generation. The facade drops pre-target video and clips audio at sample boundaries. Previously returned frames keep their old generation. Cancellation can reference the application's stop flag. A separate interrupt serial permits a new seek to interrupt blocked reads without permanently cancelling the session. Remote elementary MPEG audio uses coarse byte/duration seeking so a distant seek does not scan a missing download prefix; VBR timestamps are approximate on that path. Local MPEG audio retains accurate scanning. HTTP uses bounded request deadlines and Range resume with byte-offset validation. Remote probing omits optional trailing APE/ID3v1 scans, so cache-backed playback can open using available headers instead of waiting for the file tail; local probing retains those metadata readers.

## Native implementations

Symphonia 0.6 provides Rust demux and audio codecs, with an independent libopus adapter for Opus. The native pack routes AC-3/E-AC-3 and multichannel AAC to Windows Media Foundation or macOS AudioToolbox, preserving WAVE speaker order; mono/stereo AAC keeps the existing Symphonia path. Missing system codecs fail explicitly rather than loading an independent codec fallback. AudioToolbox is runtime-loaded from the system framework and retains supplied compressed buffers until the next input callback. Decoder drain/reset clears buffered PCM and overlap under the new generation. Audio filters and WAV muxing are Rust. MP3 encoding uses the standalone LAME library and patches a final Xing/LAME frame; it does not use an FFmpeg wrapper.

Windows video consumes demuxed compressed packets directly via IMFTransform. AVC/HEVC configuration records and length-prefixed samples are converted to Annex B inside the decoder adapter. Input/output are separate, MFT stream changes renegotiate output, and draining emits reordered B-frames. The output adapter crops alignment padding using the MFT [display aperture](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mt-minimum-display-aperture-attribute), preserving visible image dimensions in CPU NV12 planes. The current implementation selects synchronous software MFTs and CPU NV12; hardware MFT/device sharing and HDR are separate future work. Codec availability is an OS capability, with explicit failure rather than FFmpeg fallback.


macOS uses VideoToolbox through a private C bridge in its capability dylib. The core has no Apple framework dependency. Both asynchronous and temporal decode flags are clear, making callback completion part of each decode invocation; hardware selection is independent of this completion mode. Packet configuration preserves avcC/hvcC NAL prefixes. H.264 SPS/HEVC SPS parsers provide a safe presentation reorder bound, with an Annex-A DPB fallback for AVC configurations without bitstream restrictions. Retained pixel buffers remain bounded by count and bytes, are mapped only on the owning worker, then copied into independently owned CPU NV12 planes. HDR, high bit depth and interlaced input are explicitly rejected. Session recreation on seek clears references and pending pictures; generation-tagged frames remain valid after decoder close. Actual VideoToolbox hardware selection is queried instead of inferred from the request. Reconfigured dimensions update the existing ABI 2.1 output budget before a new CPU frame is copied.
