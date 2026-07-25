# build-sidecars.ps1
# Builds mkv_remux_fast.py and mkv_extract_fast.py into standalone console
# executables named with the Tauri target-triple convention.
#
# Usage (from repo root):
#   .\desktop\build-sidecars.ps1
# or from desktop/:
#   .\build-sidecars.ps1

param(
    [string]$SourceDir = $null,
    [string]$OutDir    = $null
)

$ErrorActionPreference = "Stop"

# Resolve source directory (repo root with the Python scripts)
if (-not $SourceDir) {
    # Support running from desktop/ or from repo root
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    if (Test-Path (Join-Path $scriptDir "..\mkv_remux_fast.py")) {
        $SourceDir = (Resolve-Path (Join-Path $scriptDir "..")).Path
    } elseif (Test-Path (Join-Path $scriptDir "mkv_remux_fast.py")) {
        $SourceDir = $scriptDir
    } else {
        Write-Error "Cannot find mkv_remux_fast.py. Run from repo root or pass -SourceDir."
    }
}

if (-not $OutDir) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $OutDir = Join-Path $scriptDir "src-tauri\binaries"
}

# Ensure output dir exists
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$Triple = "x86_64-pc-windows-msvc"

Write-Host "==> Building mkv-remux sidecar..." -ForegroundColor Cyan
python -m PyInstaller `
    --onefile --console `
    --name "mkv-remux-$Triple" `
    --distpath $OutDir `
    --workpath (Join-Path $env:TEMP "pyinstaller-work-remux") `
    --specpath (Join-Path $env:TEMP "pyinstaller-spec-remux") `
    --noconfirm `
    (Join-Path $SourceDir "mkv_remux_fast.py")

if ($LASTEXITCODE -ne 0) { Write-Error "PyInstaller failed for mkv-remux"; exit 1 }

Write-Host "==> Building mkv-extract sidecar..." -ForegroundColor Cyan
python -m PyInstaller `
    --onefile --console `
    --name "mkv-extract-$Triple" `
    --distpath $OutDir `
    --workpath (Join-Path $env:TEMP "pyinstaller-work-extract") `
    --specpath (Join-Path $env:TEMP "pyinstaller-spec-extract") `
    --noconfirm `
    (Join-Path $SourceDir "mkv_extract_fast.py")

if ($LASTEXITCODE -ne 0) { Write-Error "PyInstaller failed for mkv-extract"; exit 1 }

Write-Host ""
Write-Host "==> Sidecars built successfully:" -ForegroundColor Green
Get-ChildItem $OutDir -Filter "mkv-*.exe" | ForEach-Object {
    Write-Host "    $($_.FullName)  ($([math]::Round($_.Length / 1MB, 1)) MB)"
}
