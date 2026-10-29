#!/usr/bin/env python3
"""Fresh-process seek wall time and Windows working/private memory (no psutil).

Wall time includes CLI startup and exit. In-process seek-to-PCM and packet/read
counts are separately logged by system_flac_seek_decodes_only_target_packets.
"""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import platform
from pathlib import Path
import statistics
import subprocess
import time


class Memory(c.Structure):
    _fields_ = [('cb', w.DWORD), ('faults', w.DWORD), ('sizes', c.c_size_t * 9)]


class Io(c.Structure):
    _fields_ = [('read_ops', c.c_ulonglong), ('write_ops', c.c_ulonglong),
                ('other_ops', c.c_ulonglong), ('read_bytes', c.c_ulonglong),
                ('write_bytes', c.c_ulonglong), ('other_bytes', c.c_ulonglong)]


def measure(exe, fixture, target):
    memory_api = c.WinDLL('psapi', use_last_error=True).GetProcessMemoryInfo
    memory_api.argtypes = [w.HANDLE, c.POINTER(Memory), w.DWORD]
    memory_api.restype = w.BOOL
    io_api = c.WinDLL('kernel32', use_last_error=True).GetProcessIoCounters
    io_api.argtypes = [w.HANDLE, c.POINTER(Io)]
    io_api.restype = w.BOOL
    started = time.perf_counter()
    process = subprocess.Popen([str(exe), '--system-only', 'decode', str(fixture),
                                '--start-us', str(target), '--frames', '1', '--software'],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    peak_rss = peak_private = read_bytes = 0
    try:
        while process.poll() is None:
            if time.perf_counter() - started > 45:
                raise TimeoutError('FLAC CLI seek exceeded 45 seconds')
            memory = Memory(cb=c.sizeof(Memory))
            if memory_api(w.HANDLE(int(process._handle)), c.byref(memory), memory.cb):
                peak_rss = max(peak_rss, memory.sizes[0])
                peak_private = max(peak_private, memory.sizes[8])
            io = Io()
            if io_api(w.HANDLE(int(process._handle)), c.byref(io)):
                read_bytes = max(read_bytes, io.read_bytes)
            time.sleep(0.002)
        stdout, stderr = process.communicate()
        return dict(wall_ms=(time.perf_counter() - started) * 1000, peak_rss_bytes=peak_rss,
                    sampled_peak_private_bytes=peak_private, process_read_bytes=read_bytes,
                    exit_code=process.returncode, stdout=stdout.strip(), stderr=stderr.strip())
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--current', type=Path, required=True)
    parser.add_argument('--fixtures', type=Path, default=Path('target/fixtures'))
    parser.add_argument('--output', type=Path, default=Path('target/flac-seek-measurements.json'))
    args = parser.parse_args()
    report = dict(host=platform.platform(), architecture=platform.machine(), cases=[])
    for name in ('flac-long-1024.flac', 'flac-long-4096.flac', 'flac-long-table.flac', 'flac-long-variable.flac'):
        case = dict(fixture=name, duration_us=1800000000, target_us=1798000000)
        for label, exe in [('baseline', args.baseline), ('current', args.current)]:
            results = [measure(exe.resolve(), (args.fixtures / name).resolve(), case['target_us']) for _ in range(5)]
            case[label] = dict(runs=results, median_wall_ms=statistics.median(r['wall_ms'] for r in results),
                               median_peak_rss_bytes=statistics.median(r['peak_rss_bytes'] for r in results),
                               median_sampled_peak_private_bytes=statistics.median(r['sampled_peak_private_bytes'] for r in results))
            print(json.dumps(dict(fixture=name, label=label, **case[label])), flush=True)
        report['cases'].append(case)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2), encoding='utf-8')
    if any(r['exit_code'] or json.loads(r['stdout']).get('audio_frames', 0) != 1
           for case in report['cases'] for r in case['current']['runs']):
        raise SystemExit('Current FLAC seek did not produce one PCM frame')
