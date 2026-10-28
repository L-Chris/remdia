param([switch]$Release)
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    . ./scripts/setup-windows.ps1
    $profile = if ($Release) { 'release' } else { 'debug' }
    $buildArgs = @('build', '--workspace', '--locked')
    if ($Release) { $buildArgs += '--release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
    python scripts/stage.py --profile $profile
    if ($LASTEXITCODE -ne 0) { throw 'Stage failed.' }
    # DLL search is deterministic: codec DLLs accompany the executable.
    Copy-Item "$env:FFMPEG_DIR/bin/*.dll" 'dist/remdia/'
    New-Item -ItemType Directory -Force 'dist/remdia/ffmpeg-notices' | Out-Null
    Get-ChildItem $env:FFMPEG_DIR -File | Copy-Item -Destination 'dist/remdia/ffmpeg-notices/'
    if (Test-Path "$env:FFMPEG_DIR/licenses") { Copy-Item "$env:FFMPEG_DIR/licenses" 'dist/remdia/ffmpeg-notices/' -Recurse -Force }
    Copy-Item 'THIRD_PARTY.md' 'dist/remdia/ffmpeg-notices/'
    Compress-Archive -Path 'dist/remdia/*' -DestinationPath 'dist/remdia-windows-x86_64.zip' -Force
    (Get-FileHash 'dist/remdia-windows-x86_64.zip' -Algorithm SHA256).Hash.ToLowerInvariant() + '  remdia-windows-x86_64.zip' | Set-Content 'dist/SHA256SUMS.txt' -Encoding ascii
} finally { Pop-Location }
