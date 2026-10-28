#!/usr/bin/env python3
"""Generate owned FLAC constant-subframe seek fixtures without a media library.

RFC 9639 frames carry a predictable signed 16-bit constant per block, letting
tests verify the audio at the requested sample as well as timestamps/counts.
These deliberately compact files exercise long timelines, not disk throughput.
"""
import argparse
from pathlib import Path


def crc(data, bits, polynomial):
    value = 0
    for byte in data:
        value ^= byte << (bits - 8)
        for _ in range(8):
            value = ((value << 1) ^ (polynomial if value & (1 << (bits - 1)) else 0)) & ((1 << bits) - 1)
    return value


def coded_number(number):
    if number < 128:
        return bytes([number])
    count = next(n for n in range(2, 8) if number < (1 << (5 * n + 1)))
    tail = []
    for _ in range(count - 1):
        tail.insert(0, 0x80 | (number & 63))
        number >>= 6
    return bytes([(0xff << (8 - count)) & 255 | number, *tail])


def frame(first, count, index, variable):
    # Explicit 16-bit block length; 48 kHz; mono, signed 16-bit samples.
    header = bytes([0xff, 0xf8 | variable, 0x7a, 0x08])
    header += coded_number(first if variable else index) + (count - 1).to_bytes(2, 'big')
    header += bytes([crc(header, 8, 0x07)])
    value = (first // 1024) % 30000 - 15000
    data = header + b'\0' + value.to_bytes(2, 'big', signed=True)
    return data + crc(data, 16, 0x8005).to_bytes(2, 'big')


def generate(path, blocks, seektable, seconds):
    total = seconds * 48000
    variable = len(blocks) > 1
    first, index, offset = 0, 0, 0
    frames, points = [], []
    while first < total:
        count = min(blocks[index % len(blocks)], total - first)
        encoded = frame(first, count, index, variable)
        if seektable and (not points or first - points[-1][0] >= 10 * 48000):
            points.append((first, offset, count))
        frames.append(encoded)
        first += count
        offset += len(encoded)
        index += 1
    streaminfo = min(blocks).to_bytes(2, 'big') + max(blocks).to_bytes(2, 'big') + bytes(6)
    streaminfo += ((48000 << 44) | (15 << 36) | total).to_bytes(8, 'big') + bytes(16)
    with path.open('wb') as output:
        output.write(b'fLaC' + bytes([0 if seektable else 128, 0, 0, 34]) + streaminfo)
        if seektable:
            table = b''.join(sample.to_bytes(8, 'big') + byte.to_bytes(8, 'big') + size.to_bytes(2, 'big')
                             for sample, byte, size in points)
            output.write(bytes([0x83]) + len(table).to_bytes(3, 'big') + table)
        for encoded in frames:
            output.write(encoded)
    print(f'{path}: {seconds}s, {len(frames)} frames, {path.stat().st_size} bytes')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', default='target/fixtures')
    parser.add_argument('--seconds', type=int, default=1800)
    args = parser.parse_args()
    root = Path(args.output)
    root.mkdir(parents=True, exist_ok=True)
    for name, blocks, table in [
        ('flac-long-1024.flac', [1024], False),
        ('flac-long-4096.flac', [4096], False),
        ('flac-long-table.flac', [1024], True),
        ('flac-long-variable.flac', [1024, 4096, 2048], False),
    ]:
        generate(root / name, blocks, table, args.seconds)
