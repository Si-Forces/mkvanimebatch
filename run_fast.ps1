# Set console encoding to UTF-8 to handle any special characters cleanly
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# Refuse to execute an incomplete or mixed-version local installation.
function Test-RequiredFiles ([string[]]$FileNames) {
    foreach ($fileName in $FileNames) {
        $filePath = Join-Path $PSScriptRoot $fileName
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            Write-Host "[ERROR] Required file '$fileName' is missing. Re-download the complete release." -ForegroundColor Red
            return $false
        }
    }
    return $true
}

# Check if Python is installed and available in PATH
if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
    Write-Host "=====================================================================" -ForegroundColor Red
    Write-Host "[ERROR] Python was not found in your system PATH." -ForegroundColor Red
    Write-Host "Please install Python and check 'Add Python to PATH' during setup." -ForegroundColor Red
    Write-Host "=====================================================================" -ForegroundColor Red
    Read-Host "Press Enter to exit..."
    exit
}

# Initialize global flags
$DryRunMode = "OFF"
$KeepFontsMode = "OFF"

while ($true) {
    Clear-Host
    Write-Host "=====================================================================" -ForegroundColor Cyan
    Write-Host "         Anime MKV Batch Utility for Windows (High Performance)" -ForegroundColor Cyan
    Write-Host "=====================================================================" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  [1] Fast Batch Re-Mux Videos (runs mkv_remux_fast.py)"
    Write-Host "      - Merge MKVs with best-match subtitles, chapters, and cover art."
    Write-Host "      - Parallel workers and instant metadata font checks enabled."
    Write-Host ""
    Write-Host "  [2] Fast Extract MKV Tracks/Fonts (runs mkv_extract_fast.py)"
    Write-Host "      - Extracts best ASS subtitles, chapters, and cover art."
    Write-Host "      - Skip duplicate font extraction using metadata."
    Write-Host ""
    Write-Host "  [3] Toggle Dry Run Mode (Currently [$DryRunMode])"
    Write-Host "      - Preview operations without modifying any files."
    Write-Host ""
    Write-Host "  [4] Toggle Keep Fonts Mode (Currently [$KeepFontsMode])"
    Write-Host "      - Keep temporary fonts folder after successful remux."
    Write-Host ""
    Write-Host "  [0] Exit"
    Write-Host ""
    Write-Host "=====================================================================" -ForegroundColor Cyan
    Write-Host ""

    $choice = Read-Host "Enter your choice (0-4)"
    if ($choice) {
        $choice = $choice.Trim()
    }

    switch ($choice) {
        "1" {
            Clear-Host
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Write-Host "              Fast Batch Re-Mux Video Files" -ForegroundColor Cyan
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Write-Host ""
            Write-Host "[Tip] Leave inputs blank to use defaults (current directory)."
            Write-Host "      Dry Run status is currently: [$DryRunMode]"
            Write-Host ""
            $inputDir = Read-Host "Enter Input Directory (or press Enter)"

            # Ensure script components exist locally
            if (Test-RequiredFiles @("mkv_common_fast.py", "mkv_remux_fast.py")) {
                $cmdArgs = @()
                if ($inputDir -ne "") {
                    $cmdArgs += "--input-dir", $inputDir
                }
                if ($DryRunMode -eq "ON") {
                    $cmdArgs += "--dry-run"
                }
                if ($KeepFontsMode -eq "ON") {
                    $cmdArgs += "--keep-fonts"
                }

                Write-Host ""
                Write-Host "Running: python `"$PSScriptRoot\mkv_remux_fast.py`" $cmdArgs" -ForegroundColor Yellow
                Write-Host "---------------------------------------------------------------------"
                python "$PSScriptRoot\mkv_remux_fast.py" $cmdArgs
                Write-Host "---------------------------------------------------------------------"
            }
            Write-Host ""
            Read-Host "Done processing. Press Enter to return to the main menu..."
        }
        "2" {
            Clear-Host
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Write-Host "             Fast Extract MKV Tracks/Fonts" -ForegroundColor Cyan
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Write-Host ""
            Write-Host "[Tip] Leave inputs blank to use defaults (current directory)."
            Write-Host "      Dry Run status is currently: [$DryRunMode]"
            Write-Host ""
            $inputDir = Read-Host "Enter Input Directory (or press Enter)"

            # Ensure script components exist locally
            if (Test-RequiredFiles @("mkv_common_fast.py", "mkv_extract_fast.py")) {
                $cmdArgs = @()
                if ($inputDir -ne "") {
                    $cmdArgs += "--input-dir", $inputDir
                }
                if ($DryRunMode -eq "ON") {
                    $cmdArgs += "--dry-run"
                }

                Write-Host ""
                Write-Host "Running: python `"$PSScriptRoot\mkv_extract_fast.py`" $cmdArgs" -ForegroundColor Yellow
                Write-Host "---------------------------------------------------------------------"
                python "$PSScriptRoot\mkv_extract_fast.py" $cmdArgs
                Write-Host "---------------------------------------------------------------------"
            }
            Write-Host ""
            Read-Host "Done processing. Press Enter to return to the main menu..."
        }
        "3" {
            if ($DryRunMode -eq "OFF") {
                $DryRunMode = "ON"
            } else {
                $DryRunMode = "OFF"
            }
        }
        "4" {
            if ($KeepFontsMode -eq "OFF") {
                $KeepFontsMode = "ON"
            } else {
                $KeepFontsMode = "OFF"
            }
        }
        "0" {
            Clear-Host
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Write-Host "   Exiting Anime MKV Batch Utility..." -ForegroundColor Cyan
            Write-Host "=====================================================================" -ForegroundColor Cyan
            Start-Sleep -Seconds 1
            exit
        }
        default {
            Write-Host ""
            Write-Host "[ERROR] '$choice' is not a valid option. Please try again." -ForegroundColor Red
            Start-Sleep -Seconds 2
        }
    }
}
