# Renders the store images from promo.html with headless Edge (or Chrome) into browser\store\images,
# as 24-bit PNGs without alpha, which both stores want. Needs ffmpeg on PATH for the alpha strip.
#   powershell -ExecutionPolicy Bypass -File browser\store\promo\render.ps1
$ErrorActionPreference = "Stop"
$browser = @(
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw "needs Edge or Chrome" }
if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) { throw "needs ffmpeg on PATH" }

$page = ([uri](Join-Path $PSScriptRoot "promo.html")).AbsoluteUri
$out = Join-Path (Split-Path $PSScriptRoot -Parent) "images"
New-Item -ItemType Directory -Force $out | Out-Null
$profile = Join-Path $env:TEMP "stash-promo-render"

$scenes = [ordered]@{
    "screenshot-1-downloads" = @("hero", 1280, 800)
    "screenshot-2-right-click" = @("menu", 1280, 800)
    "screenshot-3-popup" = @("popup", 1280, 800)
    "screenshot-4-stack" = @("stack", 1280, 800)
    "promo-small-440x280" = @("small", 440, 280)
    "promo-marquee-1400x560" = @("marquee", 1400, 560)
}
foreach ($name in $scenes.Keys) {
    $scene, $w, $h = $scenes[$name]
    $raw = Join-Path $env:TEMP "stash-promo-$scene.png"
    Remove-Item $raw -ErrorAction SilentlyContinue
    # the browser reports progress on stderr; only the file it writes matters
    $ErrorActionPreference = "Continue"
    & $browser --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
        --user-data-dir="$profile" --allow-file-access-from-files --virtual-time-budget=4000 `
        --window-size="$w,$h" --screenshot="$raw" "$page#$scene" 2>&1 | Out-Null
    $ErrorActionPreference = "Stop"
    for ($i = 0; $i -lt 50 -and -not (Test-Path $raw); $i++) { Start-Sleep -Milliseconds 200 }
    if (-not (Test-Path $raw)) { throw "no screenshot for $scene" }
    $png = Join-Path $out "$name.png"
    ffmpeg -v error -y -i $raw -vf "crop=${w}:${h}:0:0" -pix_fmt rgb24 $png
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed on $scene" }
    Write-Host "$png"
}
