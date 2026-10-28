# Architecture

The original [proposal](remedia.md) is the direction, not a claim that every subsystem already exists.

```text
remdia-cli -> remdia (registry / plan / runtime)
                   -> remdia-types
                   -> remdia-buffer
                   -> remdia-plugin-abi
                            ^
                    remdia-backend-ffmpeg (cdylib)
                            -> libavformat / libavcodec / swresample / swscale
```

The default Cargo workspace members build the host without FFmpeg headers, linker inputs or runtime libraries. `--workspace` also builds the FFmpeg capability pack. Discovery parses platform-specific manifests without loading code. Open selects a compatible pack and lazily queries its C ABI. Modules are cached by canonical path and pinned for process lifetime. Instances are destroyed at session close; returned buffers have independent owners and remain valid.

V1 deliberately fuses source, demux, decoder and converter in one session pack. It exercises the ABI, resource and timeline contracts with replayer's workload before introducing a general graph. The planner selects task paths, thread settings and CPU output reservations. It is a structural plan; actual streams are reported after open, and actual hardware decoding is reported per frame. It is not yet a per-track graph cost optimizer or remux planner.

## Runtime

`Session::read` pulls at most one output. The backend drains the active decoder before reading the next packet, handles one-input/many-output decoding, sends EOF once to each decoder, then flushes the resampler. There is no decoded-frame background queue and no unbounded packet queue. Codec internals may retain references. Codec worker count is configured per decoder, not a hard process thread cap.

Before a plugin operation the host reserves the maximum allowed output. Each returned frame carries that reservation until its last clone is released. The plugin checks per-packet, dimensions and output sizes. This conservative accounting intentionally reserves more than many small frames require. Peak reservation is reported separately from RSS; metadata, encoded exports and external codec/driver allocation are not comprehensive process memory accounting. Chunk PCM has an additional reservation. Serialization and WAV export allocate temporary data, so the budget is not an allocator-wide hard cap.

A held frame cannot block a thread waiting for another reservation: the read fails immediately, and callers can release frames and retry without consuming another packet. Cancellation uses a stable host callback context held through plugin close. Input interrupt closures check cancellation and an operation deadline; decoder CPU work is only interrupted between API calls. Per-read deadlines also limit pathological streams that never produce matching output. Host callbacks do not panic.

## Timeline

Track time bases remain positive integer rationals. Public seek positions and output timestamps use integer microseconds, normalized against container start time; unknown video PTS is `None`. Audio output advances using integer sample indices, avoiding accumulated float rounding. Timestamp discontinuities greater than 100ms reset the resampled output anchor. Seek lands at/before a keyframe, flushes decoders and resampling, discards earlier video and clips PCM at sample boundaries. Successful seeks increment generation; previously returned frames keep their original generation.

`audio_chunk` owns an independent mono 16kHz Audio session, seeks for each range, inserts bounded silence for timestamp gaps and clips its end. Requests are limited to 120 seconds. Quiet-point chunk scheduling, overlapping AI requests and subtitle text formatting belong to replayer.

## Windows video

D3D11VA is attempted only for playback when enabled. Decoder/device creation failure falls back to software. The format callback permits FFmpeg's software choice when hardware cannot negotiate a supported format. Mid-stream hardware failures return errors; no silent codec restart occurs. Actual hardware surfaces are downloaded to CPU before publication; WGPU/D3D sharing is deferred. Software video remains native planar data. Unsupported plane layouts are converted through swscale rather than exposed with invalid strides.

## Future milestones

1. Integrate the SDK into replayer behind a feature and run its existing playback/network suite.
2. Certify real Windows GPUs and codec/profile/bit-depth combinations; measure against the same FFmpeg backend.
3. Separate component factories and add packet-copy/remux planning with explicit bitstream and timestamp negotiation.
4. Add native Media Foundation and device-domain buffers with negotiated synchronization.

No performance or memory-reduction percentage is promised before those measurements.
