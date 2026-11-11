# Gets a Chrome Web Store API refresh token for the release workflow's CHROME_REFRESH_TOKEN secret.
# Needs a Google Cloud OAuth client of type "Desktop app" (see "Publishing" in browser/README.md).
#   powershell -ExecutionPolicy Bypass -File browser\store\get-chrome-refresh-token.ps1
# Opens the Google consent page in your browser; sign in with the account that owns the
# Web Store item and approve. The token is printed here and goes nowhere else.
# -ClientSecretFile reads the secret from a file instead of asking; -OutFile writes the token
# to a file instead of printing it (e.g. to feed `gh secret set CHROME_REFRESH_TOKEN < file`).
param(
    [string]$ClientId = (Read-Host 'OAuth client ID'),
    [string]$ClientSecretFile,
    [string]$OutFile,
    [int]$Port = 8818
)
$ErrorActionPreference = 'Stop'
if ($ClientSecretFile) {
    $ClientSecret = (Get-Content -Raw $ClientSecretFile).Trim()
} else {
    $secure = Read-Host 'OAuth client secret' -AsSecureString
    $ClientSecret = [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
}

$redirect = "http://localhost:$Port/"
$scope = 'https://www.googleapis.com/auth/chromewebstore'
$url = 'https://accounts.google.com/o/oauth2/v2/auth?response_type=code&access_type=offline&prompt=consent' +
    "&scope=$([uri]::EscapeDataString($scope))&redirect_uri=$([uri]::EscapeDataString($redirect))&client_id=$([uri]::EscapeDataString($ClientId))"

$listener = New-Object System.Net.HttpListener
$listener.Prefixes.Add($redirect)
$listener.Start()
try {
    Write-Host "Opening the consent page. If it doesn't open, visit:`n$url`n"
    Start-Process $url
    $ctx = $listener.GetContext()
    $code = $ctx.Request.QueryString['code']
    $err = $ctx.Request.QueryString['error']
    $page = if ($code) { 'Got it. You can close this tab and go back to the terminal.' } else { "Sign-in failed: $err" }
    $bytes = [Text.Encoding]::UTF8.GetBytes("<html><body style='font-family:sans-serif'>$page</body></html>")
    $ctx.Response.ContentType = 'text/html; charset=utf-8'
    $ctx.Response.OutputStream.Write($bytes, 0, $bytes.Length)
    $ctx.Response.Close()
} finally {
    $listener.Stop()
}
if (-not $code) { throw "No authorization code: $err" }

$token = Invoke-RestMethod -Method Post -Uri https://oauth2.googleapis.com/token -Body @{
    client_id = $ClientId; client_secret = $ClientSecret; code = $code
    grant_type = 'authorization_code'; redirect_uri = $redirect
}
if (-not $token.refresh_token) { throw 'Google returned no refresh token' }
if ($OutFile) {
    [IO.File]::WriteAllText($OutFile, $token.refresh_token)
    Write-Host "Refresh token written to $OutFile"
} else {
    Write-Host "`nCHROME_REFRESH_TOKEN:`n$($token.refresh_token)`n"
    Write-Host 'Add it as a repository secret, then clear this terminal.'
}
