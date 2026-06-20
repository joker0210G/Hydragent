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
REM     Hydragent ui               - launch the browser Control UI
REM     Hydragent <subcommand> ... - any other subcommand of the binary
REM
REM If a .env is found at the canonical location (%HYDRAGENT_HOME%\.env,
REM top-level, NOT under data\), it is auto-loaded before the binary runs,
REM so the binary sees a fully-configured environment.
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

REM --- 2b. Special subcommand: ui ---------------------------------------------
REM `Hydragent ui` launches the browser-based Control UI (Python adapter
REM in adapters/control_ui/). This is a thin convenience wrapper around
REM `python -m adapters.control_ui`; the kernel must already be running
REM (or the UI will just show "offline" until it comes up).
if /I "%~1"=="ui" goto :do_ui
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
REM Canonical .env location: %HYDRAGENT_HOME%\.env (top-level, per paths.rs).
if exist "%HYDRAGENT_HOME%\.env" call :load_env "%HYDRAGENT_HOME%\.env"
"%HYDRAGENT_BIN_EXE%" %*
exit /b %ERRORLEVEL%

:no_args
REM Canonical .env location: %HYDRAGENT_HOME%\.env (top-level, per paths.rs).
if exist "%HYDRAGENT_HOME%\.env" (
    set "HYDRAGENT_DEFAULT_CMD=serve"
) else (
    set "HYDRAGENT_DEFAULT_CMD=onboard"
    echo.
    echo [Hydragent] No .env found at %HYDRAGENT_HOME%\.env.
    echo [Hydragent] Launching the first-run onboarding wizard...
    echo.
)
if exist "%HYDRAGENT_HOME%\.env" call :load_env "%HYDRAGENT_HOME%\.env"
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
        "try { Invoke-WebRequest -UseBasicParsing -Uri 'https://joker0210G.github.io/Hydragent/install.ps1' -OutFile '%HYDRAGENT_INSTALLER%' } catch { try { Invoke-WebRequest -UseBasicParsing -Uri 'https://raw.githubusercontent.com/joker0210G/Hydragent/main/install.ps1' -OutFile '%HYDRAGENT_INSTALLER%' } catch { exit 1 } }"
    if errorlevel 1 (
        echo [Hydragent] ERROR: Could not download install.ps1.
:do_ui
REM Launch the Control UI adapter. We try the repo layout first (so
REM contributors running from a clone get the latest code), then fall
REM back to the packaged adapter under %HYDRAGENT_HOME%\adapters.
if exist "%HYDRAGENT_HOME%\.env" call :load_env "%HYDRAGENT_HOME%\.env"
set "HYDRA_CONTROL_UI_REPO_ROOT=%HYDRAGENT_HOME%\src\adapters"
if not exist "%HYDRA_CONTROL_UI_REPO_ROOT%\control_ui" (
    set "HYDRA_CONTROL_UI_REPO_ROOT=%HYDRAGENT_HOME%\adapters"
)
where python >nul 2>nul
if errorlevel 1 (
    echo [Hydragent] ERROR: python is not on PATH. Install Python 3.10+ and try again.
    exit /b 1
)
if not exist "%HYDRAGENT_CONTROL_UI_REPO_ROOT%\control_ui\__init__.py" (
    echo [Hydragent] ERROR: Could not find adapters/control_ui at:
    echo             "%HYDRAGENT_CONTROL_UI_REPO_ROOT%"
    echo             Run `Hydragent install` to bootstrap a fresh checkout.
    exit /b 1
)
echo [Hydragent] Starting Control UI on http://%HYDRA_CONTROL_UI_HOST%:%HYDRA_CONTROL_UI_PORT%/
echo             Press Ctrl-C to stop.
set "PYTHONPATH=%HYDRAGENT_CONTROL_UI_REPO_ROOT%;%HYDRAGENT_HOME%\adapters;%PYTHONPATH%"
python -m adapters.control_ui %2 %3 %4 %5 %6 %7 %8 %9
exit /b %ERRORLEVEL%

        echo [Hydragent] Please clone the repo and run: git clone https://github.com/joker0210G/Hydragent ^&^& cd Hydragent ^&^& Hydragent.cmd install
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
