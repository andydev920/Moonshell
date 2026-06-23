# Build a Windows .exe installer (NSIS) for Moonshell on your Windows machine.
#
# Prereqs (one-time):
#   - Rust (https://rustup.rs) with the MSVC toolchain (default on Windows)
#   - Microsoft C++ Build Tools (or Visual Studio with "Desktop development with C++")
#   - Node.js 20+ and pnpm  (npm i -g pnpm)
#   - WebView2 runtime — pre-installed on Win10 21H2+/Win11; the produced installer
#     also auto-downloads it on machines that lack it (webviewInstallMode).
#
# Signing is OPTIONAL. If your Authenticode cert is installed in the Windows cert
# store, set its thumbprint to sign the installer:
#   $env:WINDOWS_CERTIFICATE_THUMBPRINT = "ABCD...EF"   (Cert:\CurrentUser\My)
#   pwsh ./scripts/build-windows.ps1
# Without it the build is UNSIGNED and SmartScreen warns ("unknown publisher").
#
# Run from the repo root or scripts/ :  pwsh ./scripts/build-windows.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

Write-Host '==> Installing frontend deps…'
pnpm install --frozen-lockfile

# --bundles nsis -> just the .exe installer. Drop the flag (or use "nsis,msi")
# if you also want the .msi (requires WiX, which Tauri downloads on demand).
$tauriArgs = @('tauri', 'build', '--bundles', 'nsis')
if ($env:WINDOWS_CERTIFICATE_THUMBPRINT) {
  Write-Host "==> Signing enabled (thumbprint $env:WINDOWS_CERTIFICATE_THUMBPRINT)…"
  $cfg = @{ bundle = @{ windows = @{
    certificateThumbprint = $env:WINDOWS_CERTIFICATE_THUMBPRINT
    digestAlgorithm = 'sha256'
    timestampUrl = 'http://timestamp.digicert.com'
  } } } | ConvertTo-Json -Depth 32 -Compress
  $tauriArgs += @('--config', $cfg)
} else {
  Write-Host '==> Building UNSIGNED (set WINDOWS_CERTIFICATE_THUMBPRINT to sign)…'
}

Write-Host '==> Building Windows NSIS installer (compiles the Rust binary)…'
pnpm @tauriArgs

$out = 'src-tauri/target/release/bundle/nsis'
Write-Host '==> Done. Artifacts:'
Get-ChildItem -Path $out -Filter *.exe -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
