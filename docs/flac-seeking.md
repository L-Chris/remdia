# Native FLAC random access

The system audio decoder remains independent of the demux node. Local native
FLAC now gets a small owned framing adapter inside the system demux pack, with
no FFmpeg, libFLAC or other third-party media dependency added. Windows still
uses `CMSFLACDecMFT`; macOS still uses AudioToolbox. For these native FLAC files,
the adapter reads STREAMINFO, bounded title/artist/album comments and cover bytes
directly. Cover decoding continues to use the system image component graph.

## Why native compressed Source Reader seek was insufficient

The Windows-only `native_flac_seek_ordering` diagnostic reads the Source Reader
directly, without any decoder or facade trimming. On Windows-2025, both owned
fixtures expose arbitrary compressed byte chunks rather than complete frames:

| Request/state | audio-tail.flac output |
| --- | --- |
| Seek to 3.35 s before first read | PTS 0, 65536 bytes beginning `fLaC` |
| Seek to 3.35 s after reading | PTS 3.8896525 s, bytes beginning inside a frame |
| Seek to 0.2 s after EOF | PTS 0.7396525 s, bytes beginning inside a frame |

Flushing before the seek produces the same results. The native compressed
output therefore does not provide the frame boundary and exact timestamp
contract needed by an independently scheduled decoder. This is distinct from
whether a Source Reader with integrated PCM decoding can seek correctly.
The evidence is reproducible with `python scripts/probe-windows-flac.py`.

The former workaround recreated both reader and decoder at sample zero, then
decoded and discarded PCM up to the target. It avoided premature EOF, but its
decode work grew with the target time. Recreating the decoder alone remains a
constant-cost way to avoid the MFT's partially initialized flush state.

## Framing and seeking

`remdia-bitstream::flac::Reader` reads STREAMINFO and bounded metadata, validates
header CRC8 and complete frame CRC16, and emits one compressed frame per packet.
PTS, DTS and duration use the sample-rate time base and actual frame/sample
numbers. Both fixed and variable block strategies are supported.

When present, SEEKTABLE provides a sparse starting point. Without it, byte-offset
bisection locates validated frames near the target, then scans a bounded nearby
range. Candidate headers inside compressed payload are rejected unless a complete
CRC16-valid frame establishes the boundary. Container seek points are validated
before use. Malformed/truncated frames, I/O errors, cancellation and packet budget
errors remain errors.

The adapter has a 64 KiB input buffer, at most 4096 sparse seek entries, and a
single compressed-frame allocation bounded by `max_packet_bytes` (at most 16 MiB).
It does not allocate a full frame index or PCM cache. Adjacent probes reuse the
input buffer. It also handles the observed AVFoundation variable-block FLAC seek
EOF and intermittent open failure on macOS 15, while retaining the system decoder.

This applies to complete local native `.flac` data, including sniffed temporary
file names. Ogg-FLAC and HTTP/Range routing are unchanged. The graph factory and
ABI are unchanged. An optional demux command `{"op":"flac.stats"}` reports this
adapter's bytes read, validated frames and sparse index entries for diagnostics;
these byte counts are not total OS disk I/O or decoder packet counts.

## Verification and measurement

`scripts/fixtures.py` generates owned 30-minute fixed 1024/4096, SEEKTABLE and
variable 1024/4096/2048-block fixtures. Their predictable PCM constants verify
the sample content at the requested position, in addition to timestamp/sample
counts. The corpus is highly compressible and does not represent disk throughput.
Existing four-second tone and three-second cover fixtures retain realistic
compressed frame payloads and a final partial block.

System component certification runs each case in a separate 60-second process.
Cases cover seek before the first read, superseded seeks, backward seek, repeated
EOF/replay, exact/beyond-duration seek, frame integrity, cancellation and limits.
The graph test logs in-process seek-to-first-PCM, actual decoder input packet
counts, and container reads, and requires the first PCM to originate near the
requested target rather than sample zero.

`scripts/measure-windows-flac.py` compares complete release CLI processes, five
runs per case, using Windows process working-set/private-memory counters. Those
wall times include startup and exit; they are not in-process seek latency.
The Windows workflow accepts an optional `flac_baseline_run` dispatch input to
download a compatible baseline package and publish raw JSON. Regular CI does
not depend on a historical artifact remaining available.

## Windows measurements, 2026-10-28

Baseline: release artifact for `5bba7ce`, which decodes from zero. Current:
`b908539`, independent system MFT decoder with native FLAC framing. Measured on
the same Windows-2025 runner (Windows Server 10.0.26100, AMD64), five fresh CLI
processes per version/case with warmed file cache, requesting one PCM frame at
29:58 in a 30-minute mono/48 kHz/16-bit synthetic file. No PCM cache is used.

| Fixture | CLI wall time median, before → after | OS peak working set median, before → after |
| --- | --- | --- |
| Fixed 1024, no table | 8713.9 → 45.2 ms | 27.46 → 19.24 MiB |
| Fixed 4096, no table | 2668.8 → 45.3 ms | 27.30 → 19.38 MiB |
| Fixed 1024, SEEKTABLE | 8697.8 → 54.4 ms | 27.52 → 19.21 MiB |
| Variable 1024/4096/2048, no table | 4594.7 → 48.8 ms | 27.55 → 19.30 MiB |

These wall times include startup and exit. Separate in-process graph measurements
on the certified Windows PR run reach first PCM in 0.77–1.65 ms, accepting two
compressed packets (30–36 bytes) at the decoder. The framing adapter reads
6,784–276,736 bytes during that first seek depending on the table/block layout.
The large synthetic compression ratio does not model seek disk throughput on
real music files, and the CLI process RSS is not whole-replayer memory usage.

The sampler polls every 2 ms. RSS uses the OS-reported lifetime peak working set;
private memory is a sampled maximum and can miss short peaks. The raw JSON
retains every run, exit status, private-memory and process-I/O counters, and the
CLI's managed-budget accounting. All 40 CLI runs produced a PCM frame.

Windows SDK ZIP: 6,110,173 → 6,128,141 bytes (+17,968 bytes / 0.29%). The system
demux DLL grows from 701,440 to 738,816 bytes. These are SDK package sizes,
not replayer installer sizes. The PR artifacts record synthetic merge revisions
`766726a` and `9a19554`; Git tree identities were verified equal to `5bba7ce`
and `b908539`, respectively.

No Cargo dependency or external codec build is added. Full clean build time was
not re-measured for this seek patch; the broader SDK build measurements remain
separate in [system-media-measurements.md](system-media-measurements.md).

- [Raw measurement data](measurements/2026-10-28-windows-flac.json)
- [Windows measurement run](https://github.com/L-Chris/remdia/actions/runs/40777914200)
- [Windows component certification](https://github.com/L-Chris/remdia/actions/runs/40777883426)
- [macOS 15 ARM/Intel certification](https://github.com/L-Chris/remdia/actions/runs/40777883474)

Additional regression cases exercise the last partial frame as the only decoder
input, and 24,000 deterministic random seeks with all four sparse caches reaching
and remaining at the 4096-entry cap.

Reference: [FLAC metadata and seek tables](https://www.rfc-editor.org/rfc/rfc9639.html#section-8.5),
[FLAC frame format](https://www.rfc-editor.org/rfc/rfc9639.html#section-9),
[Source Reader seek contract](https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsourcereader-setcurrentposition).
