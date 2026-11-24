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

    # only the package's own version (the first one): a dependency table such as
    # [dependencies.windows] has a `version =` line of its own. (The static
    # [regex]::Replace's fourth argument is RegexOptions, not a count.)
    $toml = Get-Content Cargo.toml -Raw
    $toml = [regex]::new('(?m)^version\s*=\s*"[^"]+"').Replace($toml, "version = `"$Version`"", 1)
    [IO.File]::WriteAllText("$root\Cargo.toml", $toml)

    # refresh Cargo.lock with the new version (and check it still builds)
    $ErrorActionPreference = 'Continue'
    cargo build --release 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    # git writes notices (line endings, push progress) to stderr, which Windows
    # PowerShell turns into errors: go by each command's exit code instead
    function Invoke-Git {
        git @args 2>&1 | ForEach-Object { "$_" }
        if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed" }
    }
    Invoke-Git add Cargo.toml Cargo.lock
    Invoke-Git commit -m "Release $Version"
    Invoke-Git tag -a "v$Version" -m "Stash $Version"
    Invoke-Git push origin main "v$Version"
    $ErrorActionPreference = 'Stop'
    "Pushed v$Version. GitHub Actions is building the release: https://github.com/frstycodes/stash/actions"
} finally {
    Pop-Location
}
