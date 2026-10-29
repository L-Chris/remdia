$ErrorActionPreference = 'Stop'
# The narrow MP3 profile fallback compiles LAME with the C toolchain.
# System audio/video/image packs link only Windows platform frameworks.
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust before building.' }
