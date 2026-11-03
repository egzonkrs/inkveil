# Builds a release of Inkveil into dist\, optionally signed.
#
#   .\scripts\release.ps1
#   .\scripts\release.ps1 -CertThumbprint 0123ABCD...   # certificate in CurrentUser\My or LocalMachine\My
#
# See docs\RELEASING.md.
param(
    [string]$CertThumbprint,
    [string]$TimestampUrl = "http://timestamp.digicert.com"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path $cargoBin)) {
    $env:Path = "$cargoBin;$env:Path"
}

cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "tests failed" }
cargo build --release -p inkveil-app
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$exe = Join-Path $dist "inkveil.exe"
Copy-Item (Join-Path $root "target\release\inkveil.exe") $exe -Force

if ($CertThumbprint) {
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $signtool) { throw "signtool.exe not found; install the Windows SDK" }
    & $signtool.FullName sign /sha1 $CertThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 `
        /d "Inkveil" $exe
    if ($LASTEXITCODE -ne 0) { throw "signing failed" }
    & $signtool.FullName verify /pa /v $exe
    if ($LASTEXITCODE -ne 0) { throw "signature verification failed" }
} else {
    Write-Warning "Not signed. Antivirus products and SmartScreen may flag unsigned builds."
}

$hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
"$hash  inkveil.exe" | Set-Content -Encoding ascii (Join-Path $dist "inkveil.exe.sha256")
Write-Host "Built $exe"
Write-Host "SHA-256 $hash"
