$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dependencies = Join-Path $root '.deps'
New-Item -ItemType Directory -Force -Path $dependencies | Out-Null
$archive = Join-Path $dependencies 'ffmpeg-ci.zip'
$destination = Join-Path $dependencies 'ffmpeg-ci'
$expected = '74e35908a759dbebca48c1f382ea0366f3d9d270e4bf4660503803052896b807'
$url = 'https://github.com/L-Chris/replayer/releases/download/deps-ffmpeg-9.0.2/ffmpeg-ci.zip'
if (-not (Test-Path -LiteralPath $archive) -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
    Invoke-WebRequest -Uri $url -OutFile $archive
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) { throw 'FFmpeg checksum mismatch.' }
if (-not (Test-Path -LiteralPath $destination)) { Expand-Archive -LiteralPath $archive -DestinationPath $destination }
$ffmpeg = Get-ChildItem -LiteralPath $destination -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'include/libavcodec/avcodec.h') } | Select-Object -First 1
if (-not $ffmpeg) { throw 'FFmpeg development headers missing.' }
$clang = @('C:/Program Files/LLVM/bin', (Join-Path $dependencies 'libclang')) | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'libclang.dll') } | Select-Object -First 1
if (-not $clang) { throw 'Install LLVM with libclang.dll first.' }
$env:FFMPEG_DIR = $ffmpeg.FullName
$env:LIBCLANG_PATH = $clang
$env:REMDIA_TEST_FFMPEG = Join-Path $ffmpeg.FullName 'bin/ffmpeg.exe'
$env:PATH = (Join-Path $ffmpeg.FullName 'bin') + ';' + $env:PATH
if ($env:GITHUB_ENV) {
    "FFMPEG_DIR=$env:FFMPEG_DIR" >> $env:GITHUB_ENV
    "LIBCLANG_PATH=$env:LIBCLANG_PATH" >> $env:GITHUB_ENV
    "REMDIA_TEST_FFMPEG=$env:REMDIA_TEST_FFMPEG" >> $env:GITHUB_ENV
    (Join-Path $ffmpeg.FullName 'bin') >> $env:GITHUB_PATH
}
