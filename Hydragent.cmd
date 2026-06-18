@echo off
REM ============================================================================
REM Hydragent launcher for Windows
REM
REM This script is the user-facing entry point on Windows. It lives in
REM %USERPROFILE%\.hydragent\bin and is the file the `Hydragent` command
REM resolves to. It sets up environment defaults, optionally bootstraps
REM the installer, and forwards everything else to hydragent.exe.
REM
REM Usage (from any directory):
REM     Hydragent                  - if no .env exists, run onboard; else serve
REM     Hydragent install          - (re)run the one-command installer
REM     Hydragent onboard          - run the first-run configuration wizard
REM     Hydragent serve            - start the gateway in the foreground
REM     Hydragent ps               - list running Hydragent processes
REM     Hydragent status           - one-shot status dashboard
REM     Hydragent stop [pid]      - stop a running gateway
REM     Hydragent chat             - interactive REPL
REM     Hydragent <subcommand> ... - any other subcommand of the binary
REM
REM If a .env is found in the data dir, it is auto-loaded before the
REM binary runs, so the binary sees a fully-configured environment.
REM ============================================================================

setlocal EnableExtensions EnableDelayedExpansion

REM --- 1. Locate the install root ------------------------------------------------
REM Default: %USERPROFILE%\.hydragent. Allow override via HYDRAGENT_HOME
REM for advanced users / contributors running from a clone.
if "%HYDRAGENT_HOME%"=="" (
    set "HYDRAGENT_HOME=%USERPROFILE%\.hydragent"
)

set "HYDRAGENT_BIN=%HYDRAGENT_HOME%\bin"
set "HYDRAGENT_DATA_DIR=%HYDRAGENT_HOME%\data"
set "HYDRAGENT_BIN_EXE=%HYDRAGENT_BIN%\hydragent.exe"
set "HYDRAGENT_INSTALLER=%HYDRAGENT_BIN%\install.ps1"

REM --- 2. Special subcommand: install -------------------------------------------
REM `Hydragent install` runs the one-command installer. If the binary is
REM missing, we run the installer unconditionally (it's the only way to
REM put the binary in place).
if /I "%~1"=="install" goto :do_install
if not exist "%HYDRAGENT_BIN_EXE%" (
    echo [Hydragent] hydragent.exe not found at "%HYDRAGENT_BIN_EXE%".
    echo [Hydragent] Routing to the one-command installer.
    goto :do_install
)

REM --- 3. If no args: smart default ---------------------------------------------
REM - If no .env exists in the data dir, the user has not onboarded yet.
REM   Forward to `onboard` so they get the wizard.
REM - If .env exists, launch `serve` in the foreground (the gateway
REM   daemon). Same behavior as the binary's default.
if "%~1"=="" goto :no_args

REM --- 4. Forward everything else -------------------------------------------------
REM Load .env if present, then exec the binary with all args.
if exist "%HYDRAGENT_DATA_DIR%\.env" call :load_env "%HYDRAGENT_DATA_DIR%\.env"
"%HYDRAGENT_BIN_EXE%" %*
exit /b %ERRORLEVEL%

:no_args
if exist "%HYDRAGENT_DATA_DIR%\.env" (
    set "HYDRAGENT_DEFAULT_CMD=serve"
) else (
    set "HYDRAGENT_DEFAULT_CMD=onboard"
    echo.
    echo [Hydragent] No .env found in %HYDRAGENT_DATA_DIR%.
    echo [Hydragent] Launching the first-run onboarding wizard...
    echo.
)
if "%HYDRAGENT_DEFAULT_CMD%"=="onboard" (
    if exist "%HYDRAGENT_DATA_DIR%\.env" call :load_env "%HYDRAGENT_DATA_DIR%\.env"
) else (
    if exist "%HYDRAGENT_DATA_DIR%\.env" call :load_env "%HYDRAGENT_DATA_DIR%\.env"
)
"%HYDRAGENT_BIN_EXE%" %HYDRAGENT_DEFAULT_CMD%
exit /b %ERRORLEVEL%

:do_install
REM Run the one-command installer. If install.ps1 is missing locally
REM (e.g. on a brand-new machine that only has the launcher), download
REM it from the canonical URL. The org / repo below is the only place
REM these identifiers live; flip them in one place to repoint.
if not exist "%HYDRAGENT_INSTALLER%" (
    echo [Hydragent] install.ps1 not present locally. Downloading...
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
        "try { Invoke-WebRequest -UseBasicParsing -Uri 'https://your-org.github.io/hydragent/install.ps1' -OutFile '%HYDRAGENT_INSTALLER%' } catch { try { Invoke-WebRequest -UseBasicParsing -Uri 'https://raw.githubusercontent.com/your-org/hydragent/main/install.ps1' -OutFile '%HYDRAGENT_INSTALLER%' } catch { exit 1 } }"
    if errorlevel 1 (
        echo [Hydragent] ERROR: Could not download install.ps1.
        echo [Hydragent] Please clone the repo and run: git clone https://github.com/your-org/hydragent ^&^& cd hydragent ^&^& Hydragent.cmd install
        exit /b 1
    )
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%HYDRAGENT_INSTALLER%" %*
exit /b %ERRORLEVEL%

REM --- helper: load .env into the current process env -------------------------
:load_env
set "ENV_FILE=%~1"
for /F "usebackq tokens=1,* delims==" %%A in ("%ENV_FILE%") do (
    set "LINE=%%A"
    if not "!LINE:~0,1!"=="#" if not "!LINE!"=="" (
        set "%%A=%%B"
    )
)
exit /b 0
