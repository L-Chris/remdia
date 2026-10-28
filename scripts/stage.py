#!/usr/bin/env python3
"""Stage the FFmpeg-free CLI and platform capability packs."""
import argparse
import json
import platform
import shutil
import subprocess
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
if a.target:
    arch = a.target.split('-')[0]
    os_name = 'windows' if 'windows' in a.target else 'macos' if 'apple-darwin' in a.target else 'linux'
exe = 'remdia.exe' if os_name == 'windows' else 'remdia'
def copy_binary(source, destination):
    if destination.exists():
        destination.unlink()
    shutil.copy2(source, destination)

copy_binary(build / exe, out / exe)
for pack, capabilities in [
    ('native', ['native.demux', 'native.audio.decode', 'native.audio.resample', 'native.audio.gain', 'native.video.rgba', 'native.wav.mux', 'native.mp3.encode', 'native.packet.copy', 'native.mp3.mux', 'native.file.sink']),
    ('windows', ['windows.video.decode']),
    ('macos', ['macos.video.decode']),
    ('image', [f'{os_name}.image.{op}' for op in ['decode', 'probe', 'from_video', 'scale', 'crop', 'alpha', 'orient', 'tile', 'compose', 'encode', 'capabilities']]),
    ('image_webp', ['fallback.webp.decode', 'fallback.webp.probe']),
    ('system_demux', [f'{os_name}.demux']),
    ('system_audio', [f'{os_name}.audio.decode', f'{os_name}.audio.resample'] + (['windows.mp3.encode'] if os_name == 'windows' else [])),
]:
    if pack == 'system_demux' and os_name not in ('macos', 'windows'):
        continue
    if pack == 'system_audio' and os_name not in ('windows', 'macos'):
        continue
    if pack == 'image_webp' and os_name != 'windows':
        continue
    if pack == 'image' and os_name not in ('windows', 'macos'):
        continue
    if pack in ('windows', 'macos') and pack != os_name:
        continue
    library = {'windows': f'remdia_backend_{pack}.dll', 'macos': f'libremdia_backend_{pack}.dylib', 'linux': f'libremdia_backend_{pack}.so'}[os_name]
    copy_binary(build / library, plugins / library)
    (plugins / f'{pack}.json').write_text(json.dumps(dict(id=pack, abi_major=2, abi_minor=2, os=os_name, arch=arch, library=library, capabilities=capabilities), indent=2) + '\n')
# Reject stale compatibility manifests when staging over an older build.
for old in plugins.glob('*ffmpeg*'):
    old.unlink()
for name in ['README.md', 'LICENSE', 'THIRD_PARTY.md']:
    if (root / name).exists():
        shutil.copy2(root / name, out / name)
print(out)

shutil.copytree(root / "licenses", out / "licenses", dirs_exist_ok=True)

(out / "REVISION").write_text(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip() + "\n")
