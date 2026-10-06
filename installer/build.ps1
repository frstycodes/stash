# Builds stash.exe and the installer: dist\Stash-Setup-<version>.exe
# Needs NSIS 3 (makensis): on PATH, in Program Files, or passed with -MakeNsis.
param([string]$MakeNsis)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value

if (-not $MakeNsis) {
    $found = Get-Command makensis -ErrorAction SilentlyContinue
    $MakeNsis = if ($found) { $found.Source } else { "${env:ProgramFiles(x86)}\NSIS\makensis.exe" }
}
if (-not (Test-Path $MakeNsis)) { throw "makensis not found. Install NSIS 3 (winget install NSIS.NSIS) or pass -MakeNsis <path>." }

Push-Location $root
try {
    # cargo reports progress on stderr; only its exit code means failure
    $ErrorActionPreference = 'Continue'
    cargo build --release 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    New-Item -ItemType Directory -Force "$root\dist" | Out-Null
    & $MakeNsis /V2 "/DVERSION=$version" "$root\installer\stash.nsi"
    if ($LASTEXITCODE -ne 0) { throw 'makensis failed' }
    Get-Item "$root\dist\Stash-Setup-$version.exe"
} finally {
    Pop-Location
}
