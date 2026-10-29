
`audio-tail.flac` reuses replayer's MIT-licensed four-second synthetic
`adapter.flac` tone (title QQ adapter test). It verifies system FLAC seeking
near the final partial block and repeated playback after EOF.

`scripts/flac-fixtures.py` generates four owned 30-minute mono/48 kHz/16-bit
constant-subframe files into `target/fixtures`: fixed 1024/4096 blocks, optional
SEEKTABLE, and variable 1024/4096/2048 blocks. Samples have predictable values.
The compact compressed files test timeline/seek correctness, not disk throughput.
Generation requires only Python's standard library; no external media library.
