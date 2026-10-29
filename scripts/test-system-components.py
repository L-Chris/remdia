#!/usr/bin/env python3
"""Run system component certification cases in separate, time-bounded processes."""
import subprocess
import sys

listing = subprocess.check_output(['cargo', 'test', '-p', 'remdia', '--features', 'bundled', '--lib', '--locked', '--', '--list'], text=True)
names = [line.removesuffix(': test') for line in listing.splitlines()
         if line.endswith(': test') and line.startswith(('system_audio_tests::', 'system_demux_tests::'))]
failures = []
for name in names:
    print(f'Certifying {name}', flush=True)
    try:
        result = subprocess.run(['cargo', 'test', '-p', 'remdia', '--features', 'bundled', '--lib', '--locked', name,
                                 '--', '--exact', '--include-ignored', '--nocapture'],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
        print(result.stdout, flush=True)
        if result.returncode:
            failures.append(name)
    except subprocess.TimeoutExpired as error:
        print(error.stdout.decode(errors='replace') if isinstance(error.stdout, bytes) else error.stdout or '', flush=True)
        print(f'{name}: exceeded the 60-second component certification deadline', flush=True)
        failures.append(name)
if failures:
    print('Failed component cases: ' + ', '.join(failures), file=sys.stderr)
    sys.exit(1)
