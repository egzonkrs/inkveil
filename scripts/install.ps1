# Builds Inkveil in release mode and installs it for the current user, with Start menu and desktop
# shortcuts. Run it again after pulling changes to update. No administrator rights needed.
#
#   .\scripts\install.ps1                 build and install
#   .\scripts\install.ps1 -NoBuild        install the existing target\release build
#   .\scripts\install.ps1 -Uninstall      remove the program and its shortcuts
#
# Notes are never touched: they live in %LOCALAPPDATA%\Inkveil\vault, apart from the program, and
# this script only ever reads that folder. Before replacing the program it closes Inkveil (which
# saves every note) and copies the whole vault to %LOCALAPPDATA%\Inkveil\update-backups, checking
# the copy; if that fails, nothing is installed. See "Your notes and keys" in README.md.
param([switch]$NoBuild, [switch]$Uninstall, [switch]$NoDesktopShortcut)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$installDir = Join-Path $env:LOCALAPPDATA "Programs\Inkveil"
$installedExe = Join-Path $installDir "Inkveil.exe"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Inkveil.lnk"
$desktop = Join-Path ([Environment]::GetFolderPath("Desktop")) "Inkveil.lnk"
$vaultDir = Join-Path $env:LOCALAPPDATA "Inkveil\vault"
$updateBackups = Join-Path $env:LOCALAPPDATA "Inkveil\update-backups"
# Pre-update copies kept; older ones made by this script are removed.
$keepBackups = 5

function Close-RunningInkveil {
    # Close windows politely so every note is saved, then wait for the process to exit.
    $running = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -like "inkveil*" }
    foreach ($p in $running) { $p.CloseMainWindow() | Out-Null }
    foreach ($p in $running) {
        if (-not $p.WaitForExit(15000)) { throw "Inkveil is still running; close it and try again." }
    }
}

function Backup-Vault {
    # A full copy of the vault (encrypted like the original) taken while Inkveil is closed, so a
    # new build that mishandles the vault can never cost notes or keys. Only reads $vaultDir.
    if (-not (Test-Path (Join-Path $vaultDir "vault.db"))) {
        Write-Host "No vault at $vaultDir yet; nothing to back up."
        return
    }
    $target = Join-Path $updateBackups ("vault-{0}" -f (Get-Date -Format "yyyyMMdd-HHmmss"))
    New-Item -ItemType Directory -Force $updateBackups | Out-Null
    Copy-Item $vaultDir $target -Recurse
    $source = Get-ChildItem $vaultDir -Recurse -File
    foreach ($file in $source) {
        $copy = Join-Path $target $file.FullName.Substring($vaultDir.Length)
        if (-not (Test-Path $copy) -or
                (Get-FileHash $copy).Hash -ne (Get-FileHash $file.FullName).Hash) {
            throw "The vault backup in $target does not match the vault; nothing was installed."
        }
    }
    Write-Host "Backed up the vault to $target"
    Get-ChildItem $updateBackups -Directory -Filter "vault-*" |
        Sort-Object Name -Descending |
        Select-Object -Skip $keepBackups |
        ForEach-Object { Remove-Item $_.FullName -Recurse -Force }
}

if ($Uninstall) {
    Close-RunningInkveil
    Remove-Item $startMenu, $desktop -ErrorAction SilentlyContinue
    Remove-Item $installDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Inkveil removed. Your notes are still in $vaultDir."
    return
}

if (-not $NoBuild) {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path $cargoBin)) {
        $env:Path = "$cargoBin;$env:Path"
    }
    cargo build --release -p inkveil-app
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}
$built = Join-Path $root "target\release\inkveil.exe"
if (-not (Test-Path $built)) { throw "no release build at $built" }

Close-RunningInkveil
Backup-Vault
New-Item -ItemType Directory -Force $installDir | Out-Null
Copy-Item $built $installedExe -Force

$shell = New-Object -ComObject WScript.Shell
$links = @($startMenu)
if (-not $NoDesktopShortcut) { $links += $desktop }
foreach ($path in $links) {
    $link = $shell.CreateShortcut($path)
    $link.TargetPath = $installedExe
    $link.WorkingDirectory = $installDir
    $link.IconLocation = "$installedExe,0"
    $link.Description = "Inkveil: private, encrypted notes"
    $link.Save()
}

Write-Host "Installed $installedExe"
Write-Host "Open it from the Start menu or the desktop shortcut (search for Inkveil)."
