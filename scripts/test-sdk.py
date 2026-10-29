#!/usr/bin/env python3
"""Compile an independent C host and test the complete single-library SDK."""
import argparse
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--library', type=Path, required=True)
args = parser.parse_args()
work = root / 'target/sdk-smoke'
work.mkdir(parents=True, exist_ok=True)
request = dict(source=str(root/'tests/fixtures/audio.wav'), policy='system_only', task='audio', audio_rate=16000, audio_channels=1,
               hardware=False, threads=1, timeout_ms=30000, max_frame_bytes=32*1024*1024, max_packet_bytes=8*1024*1024)
(work/'media.json').write_text(json.dumps(dict(operation='media', request=request)))
(work/'image.json').write_text(json.dumps(dict(operation='image', operations=[dict(operation='decode', settings={})])))
binary = work / ('sdk-smoke.exe' if os.name == 'nt' else 'sdk-smoke')
subprocess.run(['clang','-std=c11','-Wall','-Wextra','-Werror','-I',str(root/'include'),str(root/'tests/sdk-smoke.c'),'-o',str(binary)], check=True)
subprocess.run([str(binary),str(args.library.resolve()),str(work/'media.json'),str(work/'image.json'),str(root/'tests/fixtures/cover.png')], check=True, timeout=45)
