# Installs Hydra for the current user.
#
# Everything is per-user under HKCU and %LOCALAPPDATA%: no elevation is needed,
# and uninstalling touches nothing outside this profile.
#
#   .\install.ps1                     install from the folder this script is in
#   .\install.ps1 -Uninstall          undo it
#   .\install.ps1 -NoAutostart        install without starting at login

param(
  [switch]$Uninstall,
  [switch]$NoAutostart,
  [string]$Source = $PSScriptRoot
)

$ErrorActionPreference = "Stop"
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\Hydra"
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"

function Remove-Autostart {
  if (Get-ItemProperty -Path $RunKey -Name "Hydra" -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $RunKey -Name "Hydra"
    Write-Host "  removed the startup entry"
  }
}

if ($Uninstall) {
  Write-Host "Uninstalling Hydra"
  Get-Process hdmd -ErrorAction SilentlyContinue | Stop-Process -Force
  Remove-Autostart
  foreach ($browser in @("Google\Chrome", "Chromium", "Microsoft\Edge", "BraveSoftware\Brave-Browser", "Mozilla")) {
    $key = "HKCU:\Software\$browser\NativeMessagingHosts\com.hydradm.host"
    if (Test-Path $key) { Remove-Item $key -Recurse; Write-Host "  unregistered $browser" }
  }
  if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force; Write-Host "  removed $InstallDir" }
  Write-Host "Done. Your downloads and settings in $env:LOCALAPPDATA\Hydra were left alone."
  Write-Host "Delete that folder too if you want no trace."
  exit 0
}

# Find the binaries: beside this script, or in a cargo target directory.
$candidates = @(
  $Source,
  (Join-Path $Source "..\..\target\x86_64-pc-windows-msvc\release"),
  (Join-Path $Source "..\..\target\release")
)
$binDir = $candidates | Where-Object { Test-Path (Join-Path $_ "hdmd.exe") } | Select-Object -First 1
if (-not $binDir) {
  Write-Error "Could not find hdmd.exe. Build it with: cargo build --release --target x86_64-pc-windows-msvc"
}
$binDir = (Resolve-Path $binDir).Path
Write-Host "Installing from $binDir"

Get-Process hdmd -ErrorAction SilentlyContinue | Stop-Process -Force
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

foreach ($exe in @("hdmd.exe", "hdm.exe", "hdm-host.exe")) {
  $path = Join-Path $binDir $exe
  if (Test-Path $path) {
    Copy-Item $path $InstallDir -Force
    Write-Host "  $exe"
  }
}

# The interface and the optional plugin package sit beside the binaries, which
# is where hdmd looks for them.
$repo = (Resolve-Path (Join-Path $Source "..\..")).Path
foreach ($folder in @("ui", "python")) {
  $from = Join-Path $repo $folder
  if (Test-Path $from) {
    Copy-Item $from (Join-Path $InstallDir $folder) -Recurse -Force
    Write-Host "  $folder\"
  }
}

if (-not $NoAutostart) {
  Set-ItemProperty -Path $RunKey -Name "Hydra" -Value "`"$InstallDir\hdmd.exe`""
  Write-Host "  will start at login"
} else {
  Remove-Autostart
}

# Register the native messaging host, without extension ids for now: Chromium
# only learns its own id once the extension is loaded.
$hostScript = Join-Path $repo "packaging\native-host\install.ps1"
if (Test-Path $hostScript) {
  & $hostScript -HostBinary (Join-Path $InstallDir "hdm-host.exe")
}

Write-Host ""
Write-Host "Installed to $InstallDir"
Write-Host "Start it now with:  & '$InstallDir\hdmd.exe'"
Write-Host "Then open http://127.0.0.1:47113/"
Write-Host ""
Write-Host "For browser capture, load extensions\chromium in your browser, then re-run:"
Write-Host "  packaging\native-host\install.ps1 -ExtensionIds <the extension's id>"
