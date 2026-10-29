#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-15.0}"
profile=debug
target=$(rustc -vV | awk '/^host:/ {print $2}')
while (($#)); do
  case "$1" in
    --release) profile=release; shift ;;
    --target) target=$2; shift 2 ;;
    *) echo "Usage: $0 [--release] [--target TARGET]" >&2; exit 2 ;;
  esac
done
case "$target" in
  aarch64-apple-darwin) arch=aarch64 ;;
  x86_64-apple-darwin) arch=x86_64 ;;
  *) echo "Unsupported macOS target: $target" >&2; exit 2 ;;
esac
args=(build -p remdia-sdk -p remdia-backend-fallback-mp3-lame --locked --target "$target")
if [[ $profile == release ]]; then args+=(--release); fi
cargo "${args[@]}"
# Keep the diagnostic executable thin; the standalone SDK holds its components.
cli=(build -p remdia-cli --no-default-features --locked --target "$target")
if [[ $profile == release ]]; then cli+=(--release); fi
cargo "${cli[@]}"
python3 - <<'PY'
import shutil
shutil.rmtree('dist/remdia', ignore_errors=True)
PY
python3 scripts/stage.py --profile "$profile" --target "$target"
# ARM's linker normally adds an ad-hoc signature; Intel's does not. Sign both
# architectures explicitly before auditing and archiving the portable SDK.
for binary in dist/remdia/remdia dist/remdia/plugins/*.dylib; do
  codesign --force --sign - "$binary"
done
python3 scripts/audit-package.py dist/remdia
python3 - "$arch" <<'PY'
import hashlib, sys, zipfile
from pathlib import Path
stage = Path('dist/remdia')
archive = Path(f'dist/remdia-macos-{sys.argv[1]}.zip')
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for file in sorted(stage.rglob('*')):
        if file.is_file(): z.write(file, file.relative_to(stage))
Path(f'dist/SHA256SUMS-macos-{sys.argv[1]}.txt').write_text(
    hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
PY
