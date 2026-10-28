#!/usr/bin/env python3
"""Copy the committed synthetic corpus. No external media executable required."""
import argparse
import json
import shutil
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--output', default='target/fixtures')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
out = Path(a.output).resolve()
out.mkdir(parents=True, exist_ok=True)
for source in (root / 'tests/fixtures').iterdir():
    if source.is_file():
        shutil.copy2(source, out / source.name)
request = dict(source=str(out / 'av.mp4'), task='probe', audio_rate=48000, audio_channels=2, hardware=False, threads=2, timeout_ms=3000, max_frame_bytes=134217728, max_packet_bytes=16777216)
(out / 'probe.json').write_text(json.dumps(dict(component='fallback.demux', settings=request)))
print(out)
