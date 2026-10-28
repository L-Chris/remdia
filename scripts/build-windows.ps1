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
    if (Test-Path 'dist/remdia') { Remove-Item 'dist/remdia' -Recurse -Force }
    python scripts/stage.py --profile $profile
    if ($LASTEXITCODE -ne 0) { throw 'Stage failed.' }
    # All codec libraries are compiled into capability packs. No FFmpeg DLLs.
    python scripts/audit-package.py dist/remdia
    if ($LASTEXITCODE -ne 0) { throw 'Dependency audit failed.' }
    Compress-Archive -Path 'dist/remdia/*' -DestinationPath 'dist/remdia-windows-x86_64.zip' -Force
    (Get-FileHash 'dist/remdia-windows-x86_64.zip' -Algorithm SHA256).Hash.ToLowerInvariant() + '  remdia-windows-x86_64.zip' | Set-Content 'dist/SHA256SUMS.txt' -Encoding ascii
} finally { Pop-Location }
