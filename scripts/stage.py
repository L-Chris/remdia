#!/usr/bin/env python3
"""Stage the FFmpeg-free CLI and platform capability packs."""
import argparse
import json
import platform
import shutil
import subprocess
import ctypes
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--profile', default='debug', choices=['debug', 'release'])
p.add_argument('--target')
p.add_argument('--build-directory', type=Path, help='Cargo target directory (defaults to checkout target/)')
p.add_argument('--output', default='dist/remdia')
p.add_argument('--layout', choices=['unified','legacy'], default='unified')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
build = a.build_directory or root / 'target'
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
builtin_capabilities = []
for pack, capabilities in [
    ('native', ['native.audio.gain', 'native.video.rgba', 'native.wav.mux', 'native.packet.copy', 'native.mp3.mux', 'native.mp3.gapless_mux', 'native.file.sink']),
    ('fallback_demux', ['fallback.demux']),
    ('fallback_mp3_lame', ['fallback.mp3.encode']),
    ('windows', ['windows.video.decode']),
    ('macos', ['macos.video.decode']),
    ('image', [f'{os_name}.image.{op}' for op in ['decode', 'probe', 'from_video', 'scale', 'crop', 'alpha', 'orient', 'tile', 'compose', 'encode', 'capabilities']]),
    ('system_demux', [f'{os_name}.demux', f'{os_name}.demux.capabilities']),
    ('system_audio', [f'{os_name}.audio.decode', f'{os_name}.audio.resample', f'{os_name}.audio.capabilities'] + (['windows.mp3.encode'] if os_name == 'windows' else [])),
]:
    if pack == 'fallback_mp3_lame' and os_name != 'macos':
        continue
    if pack == 'system_demux' and os_name not in ('macos', 'windows'):
        continue
    if pack == 'system_audio' and os_name not in ('windows', 'macos'):
        continue
    if pack == 'image' and os_name not in ('windows', 'macos'):
        continue
    if pack in ('windows', 'macos') and pack != os_name:
        continue
    if a.layout == 'unified' and pack != 'fallback_mp3_lame':
        for filename in (f'{pack}.json', f'remdia_backend_{pack}.dll', f'libremdia_backend_{pack}.dylib', f'libremdia_backend_{pack}.so'):
            (plugins / filename).unlink(missing_ok=True)
        if pack != 'fallback_demux' or os_name in ('windows','macos'):
            builtin_capabilities.extend(capabilities)
        continue
    library = {'windows': f'remdia_backend_{pack}.dll', 'macos': f'libremdia_backend_{pack}.dylib', 'linux': f'libremdia_backend_{pack}.so'}[os_name]
    copy_binary(build / library, plugins / library)
    (plugins / f'{pack}.json').write_text(json.dumps(dict(id=pack, abi_major=2, abi_minor=2, os=os_name, arch=arch, library=library, capabilities=capabilities), indent=2) + '\n')
if a.layout == 'unified':
    (plugins/'sdk.json').unlink(missing_ok=True)
    library = {'windows':'remdia_sdk.dll','macos':'libremdia_sdk.dylib','linux':'libremdia_sdk.so'}[os_name]
    copy_binary(build / library, plugins / library)
    capabilities = sorted(builtin_capabilities)
    # Query native artifacts; cross-stage uses the default bundled build contract.
    host_os = {'Windows':'windows','Darwin':'macos','Linux':'linux'}[platform.system()]
    host_arch = {'arm64':'aarch64','AMD64':'x86_64','x86_64':'x86_64','aarch64':'aarch64'}[platform.machine()]
    if (os_name, arch) == (host_os, host_arch):
        class Header(ctypes.Structure):
            _fields_ = [('size',ctypes.c_uint32),('major',ctypes.c_uint16),('minor',ctypes.c_uint16)]
        class View(ctypes.Structure):
            _fields_ = [('ptr',ctypes.c_void_p),('len',ctypes.c_size_t)]
        class Output(ctypes.Structure):
            _fields_ = [('header',Header),('json',View),('planes',View*4),('owner',ctypes.c_void_p),('release',ctypes.c_void_p)]
        class Api(ctypes.Structure):
            _fields_ = [('header',Header)] + [(name,ctypes.c_void_p) for name in ('describe','create','submit','receive','drain','reset','command','close')]
        loaded = ctypes.CDLL(str((plugins/library).resolve()))
        query = loaded.remdia_plugin_query_v2
        query.argtypes = [Header,ctypes.POINTER(Api)]; query.restype = ctypes.c_int32
        api = Api(); assert query(Header(ctypes.sizeof(Api),2,2),ctypes.byref(api)) == 0
        describe = ctypes.CFUNCTYPE(ctypes.c_int32,ctypes.POINTER(Output))(api.describe)
        reply = Output(); assert describe(ctypes.byref(reply)) == 0
        try: capabilities = sorted(d['id'] for d in json.loads(ctypes.string_at(reply.json.ptr,reply.json.len)))
        finally: ctypes.CFUNCTYPE(None,ctypes.c_void_p)(reply.release)(reply.owner)
    (plugins/'sdk.json').write_text(json.dumps(dict(id='sdk',abi_major=2,abi_minor=2,os=os_name,arch=arch,library=library,capabilities=capabilities),indent=2)+'\n')
    (out/'BUNDLED.json').write_text(json.dumps(dict(layout='unified',abi_major=2,abi_minor=2,os=os_name,arch=arch,
        capabilities=capabilities,external_plugins=['fallback_mp3_lame'] if os_name=='macos' else []),indent=2)+'\n')
shutil.copytree(root / 'include', out / 'include', dirs_exist_ok=True)
# Reject stale compatibility manifests when staging over an older build.
for old in plugins.glob('*ffmpeg*'):
    old.unlink()
for pack in (['fallback_vorbis'] + (['fallback_mp3_lame'] if os_name != 'macos' else [])):
    for name in (f'{pack}.json', f'remdia_backend_{pack}.dll', f'libremdia_backend_{pack}.dylib', f'libremdia_backend_{pack}.so'):
        (plugins / name).unlink(missing_ok=True)
for name in ('image_webp.json', 'remdia_backend_image_webp.dll', 'libremdia_backend_image_webp.dylib', 'libremdia_backend_image_webp.so'):
    (plugins / name).unlink(missing_ok=True)
for old in (out / 'licenses').glob('image-webp-*'):
    old.unlink()
for name in ['README.md', 'LICENSE', 'THIRD_PARTY.md']:
    if (root / name).exists():
        shutil.copy2(root / name, out / name)
print(out)

shutil.copytree(root / "licenses", out / "licenses", dirs_exist_ok=True)

(out / "REVISION").write_text(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip() + "\n")

if os_name != 'macos':
    for name in ('LAME-COPYING.txt', 'mp3lame-encoder-LGPL-3.0.txt'):
        (out / 'licenses' / name).unlink(missing_ok=True)
