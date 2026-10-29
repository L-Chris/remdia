#!/usr/bin/env python3
"""Audit the actual AAR: declared packs, public native dependencies and 16 KB ELF."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import zipfile

def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--aar', type=Path, required=True)
    parser.add_argument('--sdk', type=Path, default=Path(os.environ.get('ANDROID_HOME', root / '.deps/android-sdk')))
    args = parser.parse_args()
    host = {'Darwin':'darwin-x86_64','Linux':'linux-x86_64','Windows':'windows-x86_64'}[platform.system()]
    tools = args.sdk / 'ndk/28.2.13676358/toolchains/llvm/prebuilt' / host / 'bin'
    readelf = tools / ('llvm-readelf.exe' if os.name == 'nt' else 'llvm-readelf')
    nm = tools / ('llvm-nm.exe' if os.name == 'nt' else 'llvm-nm')
    allowed = {'libc.so','libm.so','libdl.so','liblog.so','libandroid.so','libjnigraphics.so','libmediandk.so'}
    report = {}
    destination = root / 'target/android'
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination) as temporary, zipfile.ZipFile(args.aar) as archive:
        entries = archive.namelist()
        assert 'classes.jar' in entries and 'proguard.txt' in entries
        for manifest_path in (name for name in entries if name.endswith('/manifests.json')):
            abi = manifest_path.split('/')[-2]
            manifests = json.loads(archive.read(manifest_path))
            unified = any(pack['id'] == 'sdk' for pack in manifests)
            declared = {pack['library'] for pack in manifests} | (set() if unified else {'libremdia_android_host.so'})
            if unified: assert declared == {'libremdia_sdk.so'}, 'Unified Android SDK must contain exactly one library per ABI'
            actual = {Path(name).name for name in entries if name.startswith(f'jni/{abi}/')}
            assert actual == declared, (abi, actual, declared)
            assert not any(any(blocked in name.lower() for blocked in ('ffmpeg','avcodec','avformat','symphonia','lame','libwebp','libvorbis')) for name in actual)
            report[abi] = {}
            for library in sorted(actual):
                path = Path(temporary) / library
                path.write_bytes(archive.read(f'jni/{abi}/{library}'))
                headers = subprocess.check_output([str(readelf), '-l', '--wide', str(path)], text=True)
                aligns = [int(line.split()[-1], 16) for line in headers.splitlines() if line.strip().startswith('LOAD ')]
                assert aligns and min(aligns) >= 16384, (library, aligns)
                loads = [line.split() for line in headers.splitlines() if line.strip().startswith('LOAD ')]
                for line in headers.splitlines():
                    fields = line.split()
                    if not fields or fields[0] != 'GNU_RELRO': continue
                    start, size = int(fields[2], 16), int(fields[5], 16)
                    end = start + size
                    # A partial RELRO page may not protect following writable
                    # data; a separate LOAD containing only RELRO is also valid.
                    if end % 16384:
                        assert any(int(load[2], 16) == start and int(load[5], 16) == size for load in loads), (library, 'unaligned shared RELRO')
                        protected_end = (end + 16383) // 16384 * 16384
                        assert not any('W' in ''.join(load[6:-1]) and end <= int(load[2], 16) < protected_end for load in loads), (library, 'writable data overlaps RELRO padding')
                dynamic = subprocess.check_output([str(readelf), '-d', str(path)], text=True)
                dependencies = re.findall(r'\(NEEDED\).*\[(.*?)\]', dynamic)
                assert set(dependencies) <= allowed, (library, dependencies)
                symbols = subprocess.check_output([str(nm), '-D', '--defined-only', str(path)], text=True)
                if library in ('libremdia_backend_image.so','libremdia_backend_android.so','libremdia_android_host.so','libremdia_sdk.so'):
                    assert ' JNI_OnLoad' in symbols, (library, 'missing Java initialization')
                if library != 'libremdia_android_host.so': assert ' remdia_plugin_query_v2' in symbols
                report[abi][library] = {'minimum_load_alignment':min(aligns),'needed':dependencies,'bytes':path.stat().st_size}
    print(json.dumps(report, indent=2))

if __name__ == '__main__': main()
