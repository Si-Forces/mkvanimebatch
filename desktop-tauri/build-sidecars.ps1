# build-sidecars.ps1
# Builds mkv-cli into standalone console
# executables named with the Tauri target-triple convention.
#
# Usage (from repo root):
#   .\desktop-tauri\build-sidecars.ps1
# or from desktop-tauri/:
#   .\build-sidecars.ps1

param(
    [string]$SourceDir = $null,
    [string]$OutDir    = $null
)

$ErrorActionPreference = "Stop"

# Resolve source directory (repo root with Cargo.toml)
if (-not $SourceDir) {
    # Support running from desktop-tauri/ or from repo root
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    if (Test-Path (Join-Path $scriptDir "..\Cargo.toml")) {
        $SourceDir = (Resolve-Path (Join-Path $scriptDir "..")).Path
    } elseif (Test-Path (Join-Path $scriptDir "Cargo.toml")) {
        $SourceDir = $scriptDir
    } else {
        Write-Error "Cannot find Cargo.toml. Run from repo root or pass -SourceDir."
    }
}

if (-not $OutDir) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $OutDir = Join-Path $scriptDir "src-tauri\binaries"
}

# Ensure output dir exists
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$Triple = "x86_64-pc-windows-msvc"

Write-Host "==> Building mkv-cli in release mode..." -ForegroundColor Cyan
cargo build --release -p mkv-cli
if ($LASTEXITCODE -ne 0) { Write-Error "Cargo build failed for mkv-cli"; exit 1 }
Copy-Item (Join-Path $SourceDir "target\release\mkv-cli.exe") "$OutDir\mkv-remux-$Triple.exe" -Force
Copy-Item (Join-Path $SourceDir "target\release\mkv-cli.exe") "$OutDir\mkv-extract-$Triple.exe" -Force

Write-Host ""
Write-Host "==> Sidecars built successfully:" -ForegroundColor Green
Get-ChildItem $OutDir -Filter "mkv-*.exe" | ForEach-Object {
    Write-Host "    $($_.FullName)  ($([math]::Round($_.Length / 1MB, 1)) MB)"
}
