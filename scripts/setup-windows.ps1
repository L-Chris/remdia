$ErrorActionPreference = 'Stop'
# Only the narrow Opus fallback compiles a bundled codec using CMake.
# System audio/video/image packs link only Windows platform frameworks.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) { throw 'Install CMake before building.' }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust before building.' }
