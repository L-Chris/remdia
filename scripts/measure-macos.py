#!/usr/bin/env python3
"""Measure a macOS command and its tree, including a scoped detached compiler.

macOS physical footprint includes compressed private memory. Aggregate RSS can
double-count shared mappings. Both peaks are sampled, not kernel high-water
marks; the separate time(1) log records the foreground command's own accounting.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import threading
import time


class Usage(ctypes.Structure):
    _fields_ = [('uuid', ctypes.c_ubyte * 16)] + [
        (name, ctypes.c_uint64) for name in (
            'user', 'system', 'idle_wakes', 'interrupt_wakes', 'pageins',
            'wired', 'resident', 'footprint', 'start', 'exit')]


def process_usage(pid):
    if LIBPROC is None:
        return None
    value = Usage()
    if LIBPROC.proc_pid_rusage(pid, 0, ctypes.byref(value)):
        return None
    return value


def processes():
    output = subprocess.check_output(
        ['ps', '-axo', 'pid=,ppid=,rss=,comm='], text=True)
    result = {}
    for line in output.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) == 4:
            pid, parent, rss = map(int, fields[:3])
            result[pid] = (parent, rss * 1024, fields[3])
    return result


def swap_bytes():
    if platform.system() != 'Darwin':
        return None
    output = subprocess.check_output(['sysctl', '-n', 'vm.swapusage'], text=True)
    match = re.search(r'used =\s*([\d.]+)([MGT])', output)
    return int(float(match[1]) * 1024 ** {'M': 2, 'G': 3, 'T': 4}[match[2]]) if match else None


LIBPROC = ctypes.CDLL('/usr/lib/libproc.dylib') if platform.system() == 'Darwin' else None
if LIBPROC:
    LIBPROC.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    LIBPROC.proc_pid_rusage.restype = ctypes.c_int


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--label', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cwd', type=Path, required=True)
    parser.add_argument('--track-executable-dir', type=Path, action='append', default=[])
    parser.add_argument('--memory-limit-mib', type=int, default=6144)
    parser.add_argument('--swap-growth-limit-mib', type=int, default=512)
    parser.add_argument('--timeout', type=float, default=600)
    parser.add_argument('--interval', type=float, default=0.05)
    parser.add_argument('--settle-seconds', type=float, default=1)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if platform.system() != 'Darwin':
        parser.error('detached process accounting is currently implemented for macOS only')
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if (not command or args.interval <= 0 or args.timeout <= 0
            or args.memory_limit_mib <= 0 or args.swap_growth_limit_mib < 0
            or args.settle_seconds < 0):
        parser.error('require a command, positive limits/interval and nonnegative settle/swap limits')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    prefixes = [str(p.resolve()) + '/' for p in args.track_executable_dir]
    record = dict(label=args.label, command=command, cwd=str(args.cwd.resolve()),
                  sample_interval_seconds=args.interval, platform=platform.platform(),
                  memory_limit_bytes=args.memory_limit_mib * 1024**2,
                  peak_tree_rss_bytes=0, peak_tree_footprint_bytes=None,
                  peak_tree_processes=0, aborted=None, swap_before_bytes=swap_bytes())
    identities = {}
    stop = threading.Event()
    monitor_error = []
    root = None
    started = time.monotonic()

    def snapshot():
        rows = processes()
        owned = {root.pid} if root and root.poll() is None else set()
        usages = {}
        for pid, (_, _, exe) in rows.items():
            tagged = any(exe.startswith(prefix) for prefix in prefixes)
            if tagged or pid in identities:
                usage = process_usage(pid)
                if tagged or (usage and usage.start == identities.get(pid)):
                    owned.add(pid)
                    usages[pid] = usage
        changed = True
        while changed:
            extra = {pid for pid, (parent, _, _) in rows.items() if parent in owned}
            changed = not extra.issubset(owned)
            owned.update(extra)
        owned.discard(os.getpid())
        for pid in owned:
            usages.setdefault(pid, process_usage(pid))
            if usages[pid]:
                identities[pid] = usages[pid].start
        rss = sum(rows[pid][1] for pid in owned if pid in rows)
        footprint = (sum(u.footprint for u in usages.values() if u is not None)
                     if LIBPROC else None)
        return owned, rss, footprint

    def terminate(owned):
        # Only this command's session and processes identified by its ancestry
        # or the explicitly scoped compiler directory are ever signalled.
        if root and root.poll() is None:
            try:
                os.killpg(root.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for pid in owned:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass

    def monitor():
        last_swap = 0.0
        try:
            while not stop.is_set():
                owned, rss, footprint = snapshot()
                record['peak_tree_rss_bytes'] = max(record['peak_tree_rss_bytes'], rss)
                if footprint is not None:
                    record['peak_tree_footprint_bytes'] = max(
                        record['peak_tree_footprint_bytes'] or 0, footprint)
                record['peak_tree_processes'] = max(record['peak_tree_processes'], len(owned))
                if max(rss, footprint or 0) > record['memory_limit_bytes']:
                    record['aborted'] = 'memory_limit'
                elif time.monotonic() - started > args.timeout:
                    record['aborted'] = 'timeout'
                if time.monotonic() - last_swap >= 1:
                    current = swap_bytes()
                    if current is not None:
                        record['swap_peak_bytes'] = max(record.get('swap_peak_bytes', 0), current)
                        if current - record['swap_before_bytes'] > args.swap_growth_limit_mib * 1024**2:
                            record['aborted'] = 'host_swap_growth_limit'
                    last_swap = time.monotonic()
                if record['aborted']:
                    terminate(owned)
                    return
                stop.wait(args.interval)
        except Exception as error:
            monitor_error.append(str(error))
            terminate(snapshot()[0])

    log_path = args.output.with_suffix('.log').resolve()
    time_path = args.output.with_suffix('.time').resolve()
    timed = (['/usr/bin/time', '-l', '-o', str(time_path)] + command
             if platform.system() == 'Darwin' else command)
    with log_path.open('w') as log:
        started = time.monotonic()
        root = subprocess.Popen(timed, cwd=args.cwd, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
        thread = threading.Thread(target=monitor, daemon=True)
        thread.start()
        # A timed Popen.wait polls with up to 50 ms sleeps on POSIX. That would
        # materially bias sub-second edit/no-op timings. The independent monitor
        # enforces the timeout; the blocking wait reaps the child immediately.
        code = root.wait()
        record['wall_seconds'] = time.monotonic() - started
        stop.set()
        thread.join(timeout=3)
    # The client has exited; detached compiler/worker memory still counts.
    time.sleep(args.settle_seconds)
    owned, rss, footprint = snapshot()
    record.update(exit_code=code, idle_rss_bytes=rss, idle_footprint_bytes=footprint,
                  idle_process_count=len(owned), swap_after_bytes=swap_bytes(),
                  log_file=str(log_path), time_file=str(time_path), monitor_errors=monitor_error)
    if time_path.exists():
        text = time_path.read_text()
        maximum = re.search(r'(\d+)\s+maximum resident set size', text)
        record['foreground_max_single_process_rss_bytes'] = int(maximum[1]) if maximum else None
    args.output.write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record))
    return code if code >= 0 and not record['aborted'] and not monitor_error else 1


if __name__ == '__main__':
    raise SystemExit(main())
