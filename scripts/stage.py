#!/usr/bin/env python3
"""Stage a CLI and capability pack; FFmpeg dependencies remain optional inputs."""
import argparse
import json
import platform
import shutil
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--profile', default='debug', choices=['debug', 'release'])
p.add_argument('--target')
p.add_argument('--output', default='dist/remdia')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
build = root / 'target'
if a.target:
    build /= a.target
build /= a.profile
out = root / a.output
plugins = out / 'plugins'
plugins.mkdir(parents=True, exist_ok=True)
os_name = {'Windows': 'windows', 'Darwin': 'macos', 'Linux': 'linux'}[platform.system()]
arch = {'AMD64': 'x86_64', 'x86_64': 'x86_64', 'arm64': 'aarch64', 'aarch64': 'aarch64'}[platform.machine()]
exe = 'remdia.exe' if os_name == 'windows' else 'remdia'
library = {'windows': 'remdia_backend_ffmpeg.dll', 'macos': 'libremdia_backend_ffmpeg.dylib', 'linux': 'libremdia_backend_ffmpeg.so'}[os_name]
shutil.copy2(build / exe, out / exe)
shutil.copy2(build / library, plugins / library)
(plugins / 'ffmpeg.json').write_text(json.dumps(dict(id='ffmpeg', abi_major=1, os=os_name, arch=arch, library=library, capabilities=['probe', 'decode', 'audio', 'thumbnail', 'artwork', 'mp3']), indent=2) + '\n')
for name in ['README.md', 'LICENSE', 'THIRD_PARTY.md']:
    if (root / name).exists():
        shutil.copy2(root / name, out / name)
print(out)
