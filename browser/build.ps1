# Builds the browser extensions from browser\extension into browser\dist:
#   dist\chrome\   load unpacked in chrome://extensions (Developer mode), also Edge and Brave
#   dist\firefox\  load in about:debugging > This Firefox > Load Temporary Add-on (pick manifest.json)
#   dist\stash-chrome.zip        dist\chrome zipped (keeps the manifest key, so the unpacked id stays fixed)
#   dist\stash-chrome-store.zip  for the Chrome Web Store: same, minus the key (the Web Store rejects it)
#   dist\stash-firefox.zip       for addons.mozilla.org
# Both manifests get their version from Cargo.toml, so the extensions version with the app.
$ErrorActionPreference = "Stop"
$src = Join-Path $PSScriptRoot "extension"
$dist = Join-Path $PSScriptRoot "dist"
$cargo = Join-Path (Split-Path $PSScriptRoot -Parent) "Cargo.toml"

# Browsers want 1-4 dot-separated numbers; drop any pre-release suffix (0.3.0-beta.1 -> 0.3.0)
$version = (Select-String -Path $cargo -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $version) { throw "no version in $cargo" }
$version = ($version -split '[-+]')[0]
if ($version -notmatch '^\d+(\.\d+){0,3}$') { throw "Cargo.toml version $version isn't a valid extension version" }

Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

# Zip a folder's contents with forward-slash entry names (Windows PowerShell's Compress-Archive
# writes backslashes, which addons.mozilla.org and the Web Store can reject).
function New-Zip([string]$folder, [string]$zipPath) {
    $root = (Resolve-Path $folder).Path.TrimEnd('\') + '\'
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, 'Create')
    try {
        Get-ChildItem $folder -Recurse -File | ForEach-Object {
            $name = $_.FullName.Substring($root.Length).Replace('\', '/')
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, $name, 'Optimal') | Out-Null
        }
    } finally {
        $zip.Dispose()
    }
}

function Write-Utf8([string]$path, [string]$text) {
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding $false))
}

Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
foreach ($target in "chrome", "firefox") {
    $out = Join-Path $dist $target
    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item (Join-Path $src "*") $out -Recurse -Exclude "manifest*.json"
    $name = if ($target -eq "firefox") { "manifest.firefox.json" } else { "manifest.json" }
    $manifest = [System.IO.File]::ReadAllText((Join-Path $src $name))
    $manifest = [regex]::Replace($manifest, '("version"\s*:\s*)"[^"]*"', "`${1}""$version""", 1)
    Write-Utf8 (Join-Path $out "manifest.json") $manifest
    New-Zip $out (Join-Path $dist "stash-$target.zip")

    if ($target -eq "chrome") {
        # The Web Store assigns its own key; a manifest that carries one is rejected on upload
        $store = Join-Path $dist "chrome-store"
        Copy-Item $out $store -Recurse
        $keyless = [regex]::Replace($manifest, '(?m)^\s*"key"\s*:\s*"[^"]*",?\r?\n', '')
        if ($keyless -match '"key"\s*:') { throw "couldn't strip the key from the Chrome manifest" }
        Write-Utf8 (Join-Path $store "manifest.json") $keyless
        New-Zip $store (Join-Path $dist "stash-chrome-store.zip")
        Remove-Item $store -Recurse -Force
    }
}
Write-Host "Built $dist (extension version $version)"
