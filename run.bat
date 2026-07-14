@echo off
:: Ensure variables set within this script are local to this process
setlocal enabledelayedexpansion

:: Set window title and color scheme (sleek aqua on dark background)
title Anime MKV Batch Utility
color 0B

:: Check if Python is installed and available in PATH
where python >nul 2>nul
if %ERRORLEVEL% neq 0 (
    echo =====================================================================
    echo [ERROR] Python was not found in your system PATH.
    echo Please install Python and check "Add Python to PATH" during setup.
    echo =====================================================================
    pause
    exit /b 1
)

:: Get the directory where this batch script is located (handles spaces in path)
set "SCRIPT_DIR=%~dp0"

:: Initialize global flags (placed before :menu loop to persist state)
if not defined DRY_RUN_MODE set "DRY_RUN_MODE=OFF"

:menu
cls
echo =====================================================================
echo               Anime MKV Batch Utility for Windows
echo =====================================================================
echo.
echo   [1] Batch Re-Mux Videos (runs mkv_remux.py)
echo       - Merge MKVs with best-match subtitles, chapters, and cover art.
echo       - Automatically detects and registers fonts.
echo.
echo   [2] Extract MKV Tracks/Fonts (runs mkv_extract.py)
echo       - Extracts best ASS subtitles, chapters, and cover art.
echo       - Extract and de-duplicates embedded fonts into a shared folder.
echo.
echo   [3] Toggle Dry Run Mode (Currently [!DRY_RUN_MODE!])
echo       - Preview operations without modifying any files.
echo.
echo   [0] Exit
echo.
echo =====================================================================
echo.

:: Prompt the user for input
set "choice="
set /p choice="Enter your choice (0-3): "

:: Strip all spaces from choice to handle trailing spaces gracefully
if defined choice set "choice=%choice: =%"

:: --- MENU ROUTING ---
:: You can easily add more options in the future by defining new cases below
if not defined choice goto exit
if "%choice%"=="1" goto remux
if "%choice%"=="2" goto extract
if "%choice%"=="3" goto toggle_dry_run
if "%choice%"=="0" goto exit

:: Fallback if input is invalid
echo.
echo [ERROR] "%choice%" is not a valid option. Please try again.
ping 127.0.0.1 -n 3 >nul
goto menu


:: ─── OPTION 1: BATCH REMUX ──────────────────────────────────────────────
:remux
cls
echo =====================================================================
echo                   Batch Re-Mux Video Files
echo =====================================================================
echo.
echo [Tip] Leave inputs blank to use defaults (current directory).
echo       Dry Run status is currently: [!DRY_RUN_MODE!]
echo.
set /p input_dir="Enter Input Directory (or press Enter): "

set "opts="
if not "!input_dir!"=="" set opts=!opts! --input-dir "!input_dir!"
if "!DRY_RUN_MODE!"=="ON" set opts=!opts! --dry-run

echo.
echo Running: python mkv_remux.py !opts!
echo ---------------------------------------------------------------------
python "%SCRIPT_DIR%mkv_remux.py" !opts!
echo ---------------------------------------------------------------------
echo.
echo Done processing. Press any key to return to the main menu...
pause >nul
goto menu


:: ─── OPTION 2: EXTRACT TRACKS ───────────────────────────────────────────
:extract
cls
echo =====================================================================
echo                  Extract MKV Tracks/Fonts
echo =====================================================================
echo.
echo [Tip] Leave inputs blank to use defaults (current directory).
echo       Dry Run status is currently: [!DRY_RUN_MODE!]
echo.
set /p input_dir="Enter Input Directory (or press Enter): "

set "opts="
if not "!input_dir!"=="" set opts=!opts! --input-dir "!input_dir!"
if "!DRY_RUN_MODE!"=="ON" set opts=!opts! --dry-run

echo.
echo Running: python mkv_extract.py !opts!
echo ---------------------------------------------------------------------
python "%SCRIPT_DIR%mkv_extract.py" !opts!
echo ---------------------------------------------------------------------
echo.
echo Done processing. Press any key to return to the main menu...
pause >nul
goto menu


:: ─── OPTION 3: TOGGLE DRY RUN ───────────────────────────────────────────
:toggle_dry_run
if "!DRY_RUN_MODE!"=="OFF" (
    set "DRY_RUN_MODE=ON"
) else (
    set "DRY_RUN_MODE=OFF"
)
goto menu


:: ─── OPTION 0: EXIT ─────────────────────────────────────────────────────
:exit
cls
echo =====================================================================
echo   Exiting Anime MKV Batch Utility...
echo =====================================================================
ping 127.0.0.1 -n 2 >nul
exit /b 0
