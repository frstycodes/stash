# Cuts a release: sets the version in Cargo.toml, commits, tags vX.Y.Z and pushes.
# GitHub Actions (.github/workflows/release.yml) then builds and publishes it.
#   .\installer\release.ps1 0.3.0
param([Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+$')][string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if (git status --porcelain) { throw 'Commit or stash your changes first.' }
    if ((git rev-parse --abbrev-ref HEAD) -ne 'main') { throw 'Release from the main branch.' }
    if (git tag --list "v$Version") { throw "v$Version already exists." }

    $toml = Get-Content Cargo.toml -Raw
    $toml = [regex]::Replace($toml, '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`"", 1)
    [IO.File]::WriteAllText("$root\Cargo.toml", $toml)

    # refresh Cargo.lock with the new version (and check it still builds)
    $ErrorActionPreference = 'Continue'
    cargo build --release 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    $ErrorActionPreference = 'Stop'

    git add Cargo.toml Cargo.lock
    git commit -m "Release $Version"
    git tag -a "v$Version" -m "Stash $Version"
    git push origin main "v$Version"
    "Pushed v$Version. GitHub Actions is building the release: https://github.com/frstycodes/stash/actions"
} finally {
    Pop-Location
}
