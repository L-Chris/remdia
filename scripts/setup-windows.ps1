$ErrorActionPreference = 'Stop'
# The native packs compile vendored Opus/LAME using the C++ toolchain. Video
# decoding links only Windows Media Foundation system libraries.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) { throw 'Install CMake before building.' }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust before building.' }
