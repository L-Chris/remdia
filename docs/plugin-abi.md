# Plugin ABI v1

Canonical definitions: `crates/remdia-plugin-abi/src/lib.rs` and `include/remdia.h`.

- Symbol: `remdia_plugin_query_v1`. Query takes the host table size and ABI major/minor, returns a versioned immutable function table. Unknown major or insufficient table size is rejected before writes. Future minor versions may only append fields and must check available size.
- ABI portability is within the same OS, architecture and C platform ABI. `size_t`/usize reflects that platform. Manifests must match OS/arch before loading. Rust String/Vec/trait objects never cross the boundary.
- V1 exports fused sessions: open, next, seek/reset, artwork, bounded mono-16k f32le MP3 encoding and close. Status 0=output, 1=EOF, -1=error, -2=contained panic. No asynchronous pending state is advertised; a later component ABI must add it explicitly.
- Request and metadata schemas are the serde JSON types in remdia-types. JSON is control data only; media planes travel as borrowed byte views. `open` returns MediaInfo JSON; next/artwork/encode return tagged FrameInfo JSON; errors contain an `error` string.
- Request pointers are borrowed for the duration of the call. A session may retain only the Control context/callback, which stays valid through close. Control is callable from the session's I/O operations; it returns nonzero on cancellation.
- All successful outputs and errors start with a fresh initialized Output. A nonempty View must point to valid readable memory. Output metadata is limited to 1 MiB. Plane bytes must remain immutable until release and honor the request's size limits. PCM is little-endian packed f32; video includes stride and row count for each plane.
- `owner` and `release` transfer exactly one output reference. Hosts call release once, even on errors. Only the original module frees its allocation. Output data and callbacks may outlive the session, and the release callback must be safe on any thread. Hosts may share immutable views across threads, and may not access them after release. No host pointer can be retained by an output destructor.
- Sessions are thread confined and all session calls are serialized. close is called exactly once on a non-null successfully created handle. No call may use a closed/null handle. encode_mp3 has no session state; input is nonempty, finite f32 mono at 16kHz, at most 120 seconds.
- `seek` resets demux/decoder/resampler state but does not revoke existing outputs. Host generations prevent old frames from entering the new timeline.
- Plugins must contain Rust panics/native exceptions at every boundary, including output destructors. OOM aborts and native memory corruption cannot be contained in-process. Native plugins are trusted code, not sandboxed data.
- Code stays loaded until process exit. Releasing instances never unloads a library. Restart the host to replace an installed DLL.

FFmpeg's [send/receive contract](https://ffmpeg.org/doxygen/trunk/group__lavc__encdec.html) underpins the compatibility pack; [Rust ABI is not stable](https://doc.rust-lang.org/reference/items/external-blocks.html#abi), so only C data and function tables are exported.
