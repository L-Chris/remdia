#!/usr/bin/env python3
"""Enforce the FFmpeg-free dependency and Windows binary import boundary."""
import argparse
import json
import re
import subprocess
import shutil
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('directory')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=root))
for package in metadata['packages']:
    assert 'ffmpeg' not in package['name'].lower(), package['name']
files = list(Path(a.directory).rglob('*'))
for path in files:
    assert not re.search(r'(ffmpeg|avcodec|avformat|avutil|swresample|swscale)', path.name, re.I), path
reader = shutil.which('llvm-readobj') or shutil.which('llvm-readobj.exe')
if reader:
    for path in files:
        if path.suffix.lower() in ('.dll', '.exe'):
            imports = subprocess.check_output([reader, '--coff-imports', str(path)], text=True)
            assert not re.search(r'(avcodec|avformat|avutil|swresample|swscale)', imports, re.I), path
print('Dependency graph and package contain no FFmpeg components')
