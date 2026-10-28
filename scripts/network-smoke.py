#!/usr/bin/env python3
"""Exercise the staged CLI over a real Range-capable local HTTP source."""
import argparse
import http.server
import json
import subprocess
import threading
import time
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--exe', required=True)
p.add_argument('--fixtures', required=True)
a = p.parse_args()
exe = str(Path(a.exe).resolve())
media = (Path(a.fixtures) / 'av.mp4').read_bytes()
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        if self.path == '/blocked':
            time.sleep(2)
            self.send_error(503)
            return
        start, end = 0, len(media) - 1
        requested = self.headers.get('Range')
        if requested:
            fields = requested.removeprefix('bytes=').split('-')
            start = int(fields[0] or 0)
            if fields[1]:
                end = min(int(fields[1]), end)
        self.send_response(206 if requested else 200)
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(end - start + 1))
        self.send_header('Content-Type', 'video/mp4')
        if requested:
            self.send_header('Content-Range', f'bytes {start}-{end}/{len(media)}')
        self.end_headers()
        try:
            self.wfile.write(media[start:end + 1])
        except (BrokenPipeError, ConnectionResetError):
            pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'
try:
    result = subprocess.run([exe, 'decode', base + '/av.mp4', '--start-us', '1000000', '--frames', '1000', '--software'], capture_output=True, text=True, check=True, timeout=15)
    counts = json.loads(result.stdout)
    assert counts['video_frames'] == 48 and counts['eof'], counts
    started = time.monotonic()
    blocked = subprocess.run([exe, '--timeout-ms', '200', 'probe', base + '/blocked'], capture_output=True, timeout=5)
    assert blocked.returncode != 0 and time.monotonic() - started < 3, blocked
    print('HTTP Range seek/EOF and bounded blocked-input timeout passed')
finally:
    server.shutdown()
    server.server_close()
