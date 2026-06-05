# flint — Windows build & install script
# Usage:
#   .\scripts\install.ps1              # Build + install to ~/.local/bin
#   .\scripts\install.ps1 -Run         # Build + install + launch config TUI
#   .\scripts\install.ps1 -Dir D:\bin  # Install to custom directory

param(
    [string]$Dir = "$env:USERPROFILE\.local\bin",
    [switch]$Run
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

Write-Host "=== flint installer ===" -ForegroundColor Cyan
Write-Host ""

# ── 1. Build ──────────────────────────────────────────────────────────────
Write-Host "[1/3] Building release..." -ForegroundColor Yellow
Set-Location $ProjectRoot
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"
cargo build --release 2>&1 | ForEach-Object { Write-Host "  $_" }
$buildExit = $LASTEXITCODE
$ErrorActionPreference = $prevEAP
if ($buildExit -ne 0) {
    Write-Host "Build failed!" -ForegroundColor Red
    exit 1
}

$Binary = "$ProjectRoot\target\release\flint.exe"
if (-not (Test-Path $Binary)) {
    Write-Host "Binary not found at $Binary" -ForegroundColor Red
    exit 1
}
Write-Host "  OK: $Binary" -ForegroundColor Green

# ── 2. Install ────────────────────────────────────────────────────────────
Write-Host "[2/3] Installing to $Dir ..." -ForegroundColor Yellow
if (-not (Test-Path $Dir)) {
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
}
Copy-Item $Binary "$Dir\flint.exe" -Force
Write-Host "  OK: $Dir\flint.exe" -ForegroundColor Green

# Check PATH
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($CurrentPath -notlike "*$Dir*") {
    Write-Host ""
    Write-Host "  NOTE: $Dir is not in your PATH." -ForegroundColor DarkYellow
    Write-Host "  Add it with:" -ForegroundColor DarkYellow
    Write-Host "    [Environment]::SetEnvironmentVariable('Path', `"$CurrentPath;$Dir`", 'User')" -ForegroundColor White
    Write-Host ""
}

# ── 3. Done ───────────────────────────────────────────────────────────────
Write-Host "[3/3] Done!" -ForegroundColor Green
Write-Host ""
Write-Host "  Binary : $Dir\flint.exe" -ForegroundColor Gray
Write-Host "  Usage  : flint config" -ForegroundColor Gray
Write-Host "           flint 'your prompt here'" -ForegroundColor Gray
Write-Host ""

if ($Run) {
    Write-Host "Launching flint config..." -ForegroundColor Cyan
    & "$Dir\flint.exe" config
}
