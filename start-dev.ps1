# Builds and runs Inkveil in debug mode.
#
#   .\start-dev.ps1                 uses your real vault (%LOCALAPPDATA%\Inkveil\vault)
#   .\start-dev.ps1 -TestVault      uses a throwaway vault in .\target\dev-vault
#   .\start-dev.ps1 -AllowCapture   debug builds only: lets screenshot tools see the window
param([switch]$TestVault, [switch]$AllowCapture)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
# Terminals opened before Rust was installed don't have cargo on PATH yet.
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path $cargoBin)) {
    $env:Path = "$cargoBin;$env:Path"
}
if ($TestVault) { $env:INKVEIL_VAULT_DIR = Join-Path $PSScriptRoot "target\dev-vault" }
if ($AllowCapture) { $env:INKVEIL_DEV_ALLOW_CAPTURE = "1" }

# Build without running: if an antivirus is holding the previous inkveil.exe, cargo can link the
# new build but not copy it into place, so pick whichever binary is newer.
cargo build -p inkveil-app
$installed = Join-Path $PSScriptRoot "target\debug\inkveil.exe"
$fresh = Join-Path $PSScriptRoot "target\debug\deps\inkveil.exe"
$exe = $installed
if ((Test-Path $fresh) -and (-not (Test-Path $installed) -or
        (Get-Item $fresh).LastWriteTime -gt (Get-Item $installed).LastWriteTime)) {
    $exe = $fresh
}
if (-not (Test-Path $exe)) { throw "build failed" }
# Run a copy, so the next build can replace the binary while this instance is still open.
$runDir = Join-Path $PSScriptRoot "target\run"
New-Item -ItemType Directory -Force $runDir | Out-Null
Get-ChildItem $runDir -Filter "inkveil-*.exe" -ErrorAction SilentlyContinue |
    ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
$copy = Join-Path $runDir ("inkveil-{0}.exe" -f (Get-Date -Format "yyyyMMdd-HHmmss"))
Copy-Item $exe $copy
& $copy
