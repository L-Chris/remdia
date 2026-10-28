#!/usr/bin/env python3
"""Isolate native seek ordering in separate processes (including deadlocks)."""
import os
import subprocess

subprocess.run(['cargo', 'test', '-p', 'remdia-backend-system-demux', '--lib',
                '--locked', '--no-run'], check=True)
failures = []
for state in ('startup', 'primed', 'eof'):
    for ordering in ('direct', 'flush'):
        mode = f'{state}-{ordering}'
        print(f'Native FLAC diagnostic: {mode}', flush=True)
        try:
            result = subprocess.run(
                ['cargo', 'test', '-p', 'remdia-backend-system-demux', '--lib', '--locked',
                 'windows::seek_tests::native_flac_seek_ordering', '--', '--exact', '--ignored', '--nocapture'],
                env={**os.environ, 'REMDIA_FLAC_SEEK_MODE': mode}, timeout=45,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            print(result.stdout, flush=True)
            if result.returncode:
                failures.append(mode)
        except subprocess.TimeoutExpired as error:
            print(error.stdout.decode(errors='replace') if isinstance(error.stdout, bytes) else error.stdout or '', flush=True)
            failures.append(mode + ': timeout')
print(f'Native diagnostic failures: {failures}', flush=True)
if failures:
    raise SystemExit(1)
