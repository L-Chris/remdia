#!/usr/bin/env python3
"""Generate deterministic B-frame video, PCM, tags and cover-art test fixtures."""
import argparse
import subprocess
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--ffmpeg', default='ffmpeg')
p.add_argument('--output', default='target/fixtures')
a = p.parse_args()
out = Path(a.output).resolve()
out.mkdir(parents=True, exist_ok=True)
def run(*args):
    subprocess.run([a.ffmpeg, '-v', 'error', '-y', *map(str, args)], check=True)
run('-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=24:duration=3', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=3', '-c:v', 'mpeg4', '-bf', '2', '-c:a', 'pcm_s16le', '-metadata', 'title=Remdia fixture', out / 'av.mkv')
run('-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100:duration=3', '-c:a', 'pcm_s16le', out / 'audio.wav')
run('-f', 'lavfi', '-i', 'color=c=red:size=32x32', '-frames:v', '1', out / 'cover.png')
run('-i', out / 'audio.wav', '-i', out / 'cover.png', '-map', '0:a', '-map', '1:v', '-c:a', 'flac', '-c:v', 'copy', '-disposition:v', 'attached_pic', '-metadata', 'artist=Remdia', out / 'cover.flac')
# Non-zero container origin validates normalization rather than assuming zero PTS.
run('-i', out / 'av.mkv', '-c:v', 'mpeg2video', '-an', '-f', 'mpegts', out / 'origin.ts')
(out / 'broken.bin').write_bytes(b'not a media container\x00')
print(out)
