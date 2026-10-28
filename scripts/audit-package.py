#!/usr/bin/env python3
"""Enforce the FFmpeg-free dependency and platform binary import boundary."""
import argparse
import re
import subprocess
import struct
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('directory')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
names = re.findall(r'^name = "([^"]+)"$', (root / 'Cargo.lock').read_text(), re.M)
for name in names:
    assert 'ffmpeg' not in name.lower(), name
files = list(Path(a.directory).rglob('*'))
for path in files:
    assert not re.search(r'(ffmpeg|avcodec|avformat|avutil|swresample|swscale)', path.name, re.I), path
def imports(path):
    data = path.read_bytes()
    def u16(n): return struct.unpack_from('<H', data, n)[0]
    def u32(n): return struct.unpack_from('<I', data, n)[0]
    pe = u32(0x3c)
    assert data[pe:pe + 4] == b'PE\0\0', path
    optional = pe + 24
    directory = optional + (112 if u16(optional) == 0x20b else 96)
    section = optional + u16(pe + 20)
    sections = []
    for i in range(u16(pe + 6)):
        base = section + i * 40
        sections.append((u32(base + 12), max(u32(base + 8), u32(base + 16)), u32(base + 20)))
    def offset(rva):
        for address, size, file_at in sections:
            if address <= rva < address + size:
                return file_at + rva - address
        raise ValueError(f'Unmapped import RVA in {path}')
    rva = u32(directory + 8)
    if not rva: return []
    at = offset(rva)
    result = []
    while any(data[at:at + 20]):
        name = offset(u32(at + 12))
        end = data.index(b'\0', name, name + 1024)
        result.append(data[name:end].decode('ascii'))
        at += 20
    return result

for path in files:
    if path.suffix.lower() in ('.dll', '.exe'):
        linked = imports(path)
        assert not any(re.search(r'(avcodec|avformat|avutil|swresample|swscale|vcruntime|msvcp|msvcr\d)', item, re.I) for item in linked), (path, linked)
    elif path.is_file() and (path.suffix == '.dylib' or path.name == 'remdia'):
        lines = subprocess.check_output(['otool','-L',str(path)],text=True).splitlines()[1:]
        own = subprocess.check_output(['otool','-D',str(path)],text=True).splitlines()[1:]
        own = own[0].strip() if own else None
        for line in lines:
            dep = line.strip().split(' (compatibility version',1)[0]
            if dep == own: continue
            assert dep.startswith(('/System/Library/','/usr/lib/')), (path,dep)
            assert not re.search(r'(ffmpeg|avcodec|avformat|avutil|swresample|swscale)',dep,re.I),(path,dep)
        subprocess.check_call(['codesign','--verify','--strict',str(path)])
print('Dependency graph and packaged binaries are FFmpeg-free; only platform runtime dependencies')
