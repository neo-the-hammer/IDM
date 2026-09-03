# Registers Hydra's native messaging host with the browsers on this machine.
#
# On Windows the manifest is pointed at by a registry key rather than dropped
# into a directory, so this writes under HKCU -- no administrator rights needed
# and nothing is left behind for other users.
#
# Usage: .\install.ps1 [-HostBinary path\to\hdm-host.exe] [-ExtensionIds id1,id2]

param(
  [string]$HostBinary = "",
  [string[]]$ExtensionIds = @(),
  [string]$FirefoxId = "hydra@hydradm.org"
)

$ErrorActionPreference = "Stop"

if (-not $HostBinary) {
  $candidates = @(
    (Join-Path $PSScriptRoot "..\..\target\release\hdm-host.exe"),
    (Join-Path $PSScriptRoot "..\..\target\debug\hdm-host.exe")
  )
  $HostBinary = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $HostBinary -or -not (Test-Path $HostBinary)) {
  Write-Error "Could not find hdm-host.exe. Build it with 'cargo build --release', or pass -HostBinary."
}
$HostBinary = (Resolve-Path $HostBinary).Path

# The manifest lives next to the binary; the registry only points at it.
$manifestDir = Split-Path $HostBinary
$chromeManifest = Join-Path $manifestDir "com.hydradm.host.chrome.json"
$firefoxManifest = Join-Path $manifestDir "com.hydradm.host.firefox.json"

$origins = @($ExtensionIds | ForEach-Object { "chrome-extension://$_/" })

@{
  name = "com.hydradm.host"
  description = "Hydra Download Manager native host"
  path = $HostBinary
  type = "stdio"
  allowed_origins = $origins
} | ConvertTo-Json -Depth 4 | Set-Content -Path $chromeManifest -Encoding UTF8

@{
  name = "com.hydradm.host"
  description = "Hydra Download Manager native host"
  path = $HostBinary
  type = "stdio"
  allowed_extensions = @($FirefoxId)
} | ConvertTo-Json -Depth 4 | Set-Content -Path $firefoxManifest -Encoding UTF8

function Register-Host($keyPath, $manifest) {
  New-Item -Path $keyPath -Force | Out-Null
  Set-ItemProperty -Path $keyPath -Name "(Default)" -Value $manifest
  Write-Host "  registered $keyPath"
}

Write-Host "Registering $HostBinary"

if ($origins.Count -gt 0) {
  foreach ($browser in @("Google\Chrome", "Chromium", "Microsoft\Edge", "BraveSoftware\Brave-Browser")) {
    Register-Host "HKCU:\Software\$browser\NativeMessagingHosts\com.hydradm.host" $chromeManifest
  }
} else {
  Write-Host "  (no -ExtensionIds given; skipping Chromium)"
  Write-Host "  Load the extension, copy its id from chrome://extensions, then re-run with -ExtensionIds <id>"
}

Register-Host "HKCU:\Software\Mozilla\NativeMessagingHosts\com.hydradm.host" $firefoxManifest

Write-Host "Done. Restart the browser for it to notice."
