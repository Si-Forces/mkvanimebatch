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
if not defined KEEP_FONTS_MODE set "KEEP_FONTS_MODE=OFF"
set "_empty_cnt=0"

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
echo   [4] Toggle Keep Fonts Mode (Currently [!KEEP_FONTS_MODE!])
echo       - Keep temporary fonts folder after successful remux.
echo.
echo   [0] Exit
echo.
echo =====================================================================
echo.

:: Prompt the user for input
set "choice="
set /p choice="Enter your choice (0-4): "

:: Strip quotes and spaces from choice (delayed expansion: injection-safe)
if defined choice set choice=!choice:"=!
if defined choice set "choice=!choice: =!"

:: --- MENU ROUTING ---
if not defined choice (
    set /a _empty_cnt+=1
    if !_empty_cnt! gtr 3 goto exit
    goto menu
)
set "_empty_cnt=0"
if "!choice!"=="1" goto remux
if "!choice!"=="2" goto extract
if "!choice!"=="3" goto toggle_dry_run
if "!choice!"=="4" goto toggle_keep_fonts
if "!choice!"=="0" goto exit

:: Fallback if input is invalid
echo.
echo [ERROR] "!choice!" is not a valid option. Please try again.
timeout /t 2 /nobreak >nul 2>nul || ping 127.0.0.1 -n 3 >nul
goto menu


rem --- OPTION 1: BATCH REMUX ---
:remux
cls
echo =====================================================================
echo                   Batch Re-Mux Video Files
echo =====================================================================
echo.
echo [Tip] Leave inputs blank to use defaults (current directory).
echo       Dry Run status is currently: [!DRY_RUN_MODE!]
echo.
if not exist "%SCRIPT_DIR%mkv_domain.py" (
    echo [ERROR] Required file 'mkv_domain.py' is missing.
    pause
    goto menu
)
if not exist "%SCRIPT_DIR%mkv_assets.py" (
    echo [ERROR] Required file 'mkv_assets.py' is missing.
    pause
    goto menu
)
if not exist "%SCRIPT_DIR%mkv_remux.py" (
    echo [ERROR] Required file 'mkv_remux.py' is missing.
    pause
    goto menu
)
set "input_dir="
set "out_dir="
set "fonts_dir="
set "complexity="
set "io_jobs="
set /p input_dir="Enter Input Directory (or press Enter): "
set /p out_dir="Enter Output Directory (or press Enter): "
set /p fonts_dir="Enter Fonts Directory override (or press Enter): "
set /p complexity="Enter Opus Complexity [0-10] (or press Enter for default 10): "
set /p io_jobs="Enter Concurrent I/O Jobs (or press Enter for default 2): "

:: Strip double quotes from inputs (prevents argument injection; handles pasted quoted paths)
if defined input_dir set input_dir=!input_dir:"=!
if defined out_dir set out_dir=!out_dir:"=!
if defined fonts_dir set fonts_dir=!fonts_dir:"=!
if defined complexity set complexity=!complexity:"=!
if defined io_jobs set io_jobs=!io_jobs:"=!

set "opts="
if not "!input_dir!"=="" set opts=!opts! --input-dir "!input_dir!"
if not "!out_dir!"=="" set opts=!opts! --out-dir "!out_dir!"
if not "!fonts_dir!"=="" set opts=!opts! --fonts-dir "!fonts_dir!"
if not "!complexity!"=="" (
    for /f "delims=0123456789" %%a in ("!complexity!") do (
        echo [ERROR] Complexity must be numeric (0-10).
        pause
        goto menu
    )
    set opts=!opts! --complexity "!complexity!"
)
if not "!io_jobs!"=="" (
    for /f "delims=0123456789" %%a in ("!io_jobs!") do (
        echo [ERROR] I/O jobs must be numeric.
        pause
        goto menu
    )
    set opts=!opts! --io-jobs "!io_jobs!"
)
if "!DRY_RUN_MODE!"=="ON" set opts=!opts! --dry-run
if "!KEEP_FONTS_MODE!"=="ON" set opts=!opts! --keep-fonts

echo.
echo Running: python mkv_remux.py !opts!
echo ---------------------------------------------------------------------
python "%SCRIPT_DIR%mkv_remux.py" !opts!
echo ---------------------------------------------------------------------
echo.
echo Done processing. Press any key to return to the main menu...
pause >nul
goto menu


rem --- OPTION 2: EXTRACT TRACKS ---
:extract
cls
echo =====================================================================
echo                  Extract MKV Tracks/Fonts
echo =====================================================================
echo.
echo [Tip] Leave inputs blank to use defaults (current directory).
echo       Dry Run status is currently: [!DRY_RUN_MODE!]
echo.
if not exist "%SCRIPT_DIR%mkv_domain.py" (
    echo [ERROR] Required file 'mkv_domain.py' is missing.
    pause
    goto menu
)
if not exist "%SCRIPT_DIR%mkv_extract.py" (
    echo [ERROR] Required file 'mkv_extract.py' is missing.
    pause
    goto menu
)
set "input_dir="
set "out_dir="
set "io_jobs="
set /p input_dir="Enter Input Directory (or press Enter): "
set /p out_dir="Enter Output Directory for extracted files (or press Enter): "
set /p io_jobs="Enter Concurrent I/O Jobs (or press Enter for default 2): "

:: Strip double quotes from inputs (prevents argument injection; handles pasted quoted paths)
if defined input_dir set input_dir=!input_dir:"=!
if defined out_dir set out_dir=!out_dir:"=!
if defined io_jobs set io_jobs=!io_jobs:"=!

set "opts="
if not "!input_dir!"=="" set opts=!opts! --input-dir "!input_dir!"
if not "!out_dir!"=="" set opts=!opts! --out-dir "!out_dir!"
if not "!io_jobs!"=="" (
    for /f "delims=0123456789" %%a in ("!io_jobs!") do (
        echo [ERROR] I/O jobs must be numeric.
        pause
        goto menu
    )
    set opts=!opts! --io-jobs "!io_jobs!"
)
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


rem --- OPTION 3: TOGGLE DRY RUN ---
:toggle_dry_run
if "!DRY_RUN_MODE!"=="OFF" (
    set "DRY_RUN_MODE=ON"
) else (
    set "DRY_RUN_MODE=OFF"
)
goto menu


rem --- OPTION 4: TOGGLE KEEP FONTS ---
:toggle_keep_fonts
if "!KEEP_FONTS_MODE!"=="OFF" (
    set "KEEP_FONTS_MODE=ON"
) else (
    set "KEEP_FONTS_MODE=OFF"
)
goto menu


rem --- OPTION 0: EXIT ---
:exit
cls
echo =====================================================================
echo   Exiting Anime MKV Batch Utility...
echo =====================================================================
ping 127.0.0.1 -n 2 >nul
exit /b 0
