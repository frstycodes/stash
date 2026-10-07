# Builds the Microsoft Store package: dist\Stash-<version>.msix
# The Store signs it on upload, so it isn't signed here.
# Needs the Windows SDK (makeappx.exe, makepri.exe).
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
$identity = Get-Content "$root\packaging\identity.json" -Raw | ConvertFrom-Json

# newest SDK tools
$sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $sdk) { throw 'makeappx.exe not found. Install the Windows SDK.' }
$makeappx = $sdk.FullName
$makepri = Join-Path $sdk.DirectoryName 'makepri.exe'

Push-Location $root
try {
    $ErrorActionPreference = 'Continue'
    cargo build --release 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    $ErrorActionPreference = 'Stop'

    $stage = Join-Path $root 'target\msix'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force "$stage\Assets" | Out-Null
    Copy-Item target\release\stash.exe, LICENSE.md $stage

    # Tile and taskbar icons, rendered from the 1024 px app icon. Scale variants keep
    # them sharp on high-DPI screens; "unplated" variants sit on the taskbar without
    # a colored plate behind them.
    Add-Type -AssemblyName System.Drawing
    $icon = [System.Drawing.Image]::FromFile("$root\assets\icon.png")
    function Save-Icon([string]$name, [int]$canvas, [double]$fill) {
        $bmp = New-Object System.Drawing.Bitmap $canvas, $canvas
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.InterpolationMode = 'HighQualityBicubic'; $g.PixelOffsetMode = 'HighQuality'; $g.SmoothingMode = 'HighQuality'
        $size = [int][Math]::Round($canvas * $fill); $off = [int](($canvas - $size) / 2)
        $g.DrawImage($icon, $off, $off, $size, $size); $g.Dispose()
        $bmp.Save("$stage\Assets\$name", [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    }
    foreach ($s in @(@(100, 1.0), @(200, 2.0), @(400, 4.0))) {
        Save-Icon "Square44x44Logo.scale-$($s[0]).png" ([int](44 * $s[1])) 1.0
        Save-Icon "Square150x150Logo.scale-$($s[0]).png" ([int](150 * $s[1])) 0.66
        Save-Icon "StoreLogo.scale-$($s[0]).png" ([int](50 * $s[1])) 1.0
    }
    foreach ($t in 16, 20, 24, 32, 40, 48, 64, 256) {
        Save-Icon "Square44x44Logo.targetsize-$t.png" $t 1.0
        Save-Icon "Square44x44Logo.targetsize-${t}_altform-unplated.png" $t 1.0
        Save-Icon "Square44x44Logo.targetsize-${t}_altform-lightunplated.png" $t 1.0
    }
    $icon.Dispose()

    $manifest = (Get-Content "$root\packaging\AppxManifest.xml" -Raw).
        Replace('{{NAME}}', $identity.name).
        Replace('{{PUBLISHER}}', $identity.publisher).
        Replace('{{PUBLISHER_DISPLAY_NAME}}', $identity.publisherDisplayName).
        Replace('{{DISPLAY_NAME}}', $identity.displayName).
        Replace('{{VERSION}}', $version)
    [IO.File]::WriteAllText("$stage\AppxManifest.xml", $manifest)

    # resources.pri maps the plain asset names in the manifest to the variants above
    $pri = Join-Path $root 'target\priconfig.xml'
    & $makepri createconfig /cf $pri /dq en-US /pv 10.0.0 /o | Out-Null
    & $makepri new /pr $stage /cf $pri /mn "$stage\AppxManifest.xml" /of "$stage\resources.pri" /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'makepri failed' }

    New-Item -ItemType Directory -Force "$root\dist" | Out-Null
    $out = "$root\dist\Stash-$version.msix"
    & $makeappx pack /d $stage /p $out /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'makeappx failed' }
    Get-Item $out
} finally {
    Pop-Location
}
