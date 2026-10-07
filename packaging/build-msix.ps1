# Builds the Microsoft Store package. The Store signs it on upload, so it isn't signed here.
#   .\packaging\build-msix.ps1                 dist\Stash-<version>.msix (x64)
#   .\packaging\build-msix.ps1 -Arch x64,arm64 dist\Stash-<version>.msixbundle (both)
#   -OutDir <folder> writes there instead of dist
# Needs the Windows SDK (makeappx.exe, makepri.exe). Arm64 also needs the Rust target
# (rustup target add aarch64-pc-windows-msvc) and Visual Studio's ARM64 build tools.
param([ValidateSet('x64', 'arm64')][string[]]$Arch = @('x64'), [string]$OutDir)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
$identity = Get-Content "$root\packaging\identity.json" -Raw | ConvertFrom-Json
$targets = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }

# newest SDK tools
$sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $sdk) { throw 'makeappx.exe not found. Install the Windows SDK.' }
$makeappx = $sdk.FullName
$makepri = Join-Path $sdk.DirectoryName 'makepri.exe'

# Tile and taskbar icons, rendered from the 1024 px app icon. Scale variants keep them
# sharp on high-DPI screens; "unplated" variants sit on the taskbar without a colored
# plate behind them.
function Write-Icons([string]$dir) {
    Add-Type -AssemblyName System.Drawing
    $icon = [System.Drawing.Image]::FromFile("$root\assets\icon.png")
    function Save-Icon([string]$name, [int]$canvas, [double]$fill) {
        $bmp = New-Object System.Drawing.Bitmap $canvas, $canvas
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.InterpolationMode = 'HighQualityBicubic'; $g.PixelOffsetMode = 'HighQuality'; $g.SmoothingMode = 'HighQuality'
        $size = [int][Math]::Round($canvas * $fill); $off = [int](($canvas - $size) / 2)
        $g.DrawImage($icon, $off, $off, $size, $size); $g.Dispose()
        $bmp.Save("$dir\$name", [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
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
}

Push-Location $root
try {
    $work = Join-Path $root 'target\msix'
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    $packages = Join-Path $work 'packages'
    if (-not $OutDir) { $OutDir = Join-Path $root 'dist' }
    New-Item -ItemType Directory -Force $packages, $OutDir | Out-Null

    foreach ($a in $Arch) {
        $target = $targets[$a]
        $ErrorActionPreference = 'Continue'
        cargo build --release --target $target 2>&1 | ForEach-Object { "$_" }
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $target" }
        $ErrorActionPreference = 'Stop'

        $stage = Join-Path $work $a
        New-Item -ItemType Directory -Force "$stage\Assets" | Out-Null
        Copy-Item "target\$target\release\stash.exe", LICENSE.md $stage
        Write-Icons "$stage\Assets"

        $manifest = (Get-Content "$root\packaging\AppxManifest.xml" -Raw).
            Replace('{{NAME}}', $identity.name).
            Replace('{{PUBLISHER}}', $identity.publisher).
            Replace('{{PUBLISHER_DISPLAY_NAME}}', $identity.publisherDisplayName).
            Replace('{{DISPLAY_NAME}}', $identity.displayName).
            Replace('{{VERSION}}', $version).
            Replace('{{ARCH}}', $a)
        [IO.File]::WriteAllText("$stage\AppxManifest.xml", $manifest)

        # resources.pri maps the plain asset names in the manifest to the variants above
        $pri = Join-Path $work "priconfig-$a.xml"
        & $makepri createconfig /cf $pri /dq en-US /pv 10.0.0 /o | Out-Null
        & $makepri new /pr $stage /cf $pri /mn "$stage\AppxManifest.xml" /of "$stage\resources.pri" /o | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "makepri failed for $a" }

        & $makeappx pack /d $stage /p "$packages\Stash-$version-$a.msix" /o | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed for $a" }
    }

    if ($Arch.Count -eq 1) {
        $out = "$OutDir\Stash-$version.msix"
        Copy-Item "$packages\Stash-$version-$($Arch[0]).msix" $out -Force
    } else {
        # one upload that the Store splits per device: native on x64 and on Arm64
        $out = "$OutDir\Stash-$version.msixbundle"
        & $makeappx bundle /d $packages /p $out /bv "$version.0" /o | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'makeappx bundle failed' }
    }
    Get-Item $out
} finally {
    Pop-Location
}
