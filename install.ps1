<#
.SYNOPSIS
    One-command installer for Hydragent (Windows).

.DESCRIPTION
    Downloads the latest prebuilt release of hydragent.exe (or builds
    from source if no release is available yet), places it in
    %USERPROFILE%\.hydragent\bin, drops a Hydragent.cmd launcher on the
    user PATH, creates a data directory, and launches the first-run
    onboarding wizard.

    The installer is idempotent: re-running it updates an existing
    installation in place. A previous install is detected by the
    presence of hydragent.exe in the bin directory.

    Mirrors the Ollama / OpenClaw one-liner pattern:

        irm https://joker0210G.github.io/Hydragent/install.ps1 | iex

    The script is self-contained: it uses only built-in PowerShell
    cmdlets (no external modules, no admin elevation).

.PARAMETER Source
    Force a from-source build. The installer will download rustup if
    cargo is not on PATH, clone the repo into %USERPROFILE%\.hydragent\src,
    and `cargo build --release -p hydragent-core`. By default the
    installer tries to download a prebuilt release first and only falls
    back to source if that fails.

.PARAMETER SkipOnboard
    Don't run `hydragent onboard` at the end. Useful for CI / scripted
    provisioning where the .env will be written separately.

.PARAMETER Force
    Overwrite an existing installation in %USERPROFILE%\.hydragent. By
    default, if hydragent.exe is already present, the installer exits
    with a friendly "already installed" message and just re-runs the
    PATH / launcher steps.

.PARAMETER Version
    Pin a specific release tag, e.g. "v0.7.2". Default: "latest",
    which resolves via the GitHub Releases API.

.PARAMETER InstallRoot
    Override the install location. Default: %USERPROFILE%\.hydragent.

.PARAMETER Repo
    GitHub "owner/repo" used to locate the install script, releases, and
    (optionally) the source tree. Default: "joker0210G/Hydragent".
    Override for forks / private mirrors.

    From this single parameter, the installer derives:
      - The install script URL  : https://joker0210G.github.io/Hydragent/install.ps1
      - The release downloads   : https://github.com/joker0210G/Hydragent/releases/...
      - The source clone        : https://github.com/joker0210G/Hydragent.git

    This means swapping the default org/repo to your own in *one* place
    is enough to make the one-liner work for your users.

.PARAMETER Quiet
    Suppress the colored banner. Useful for CI logs.

.EXAMPLE
    irm https://joker0210G.github.io/Hydragent/install.ps1 | iex

.EXAMPLE
    irm https://joker0210G.github.io/Hydragent/install.ps1 | iex -SkipOnboard

.EXAMPLE
    iwr -useb https://joker0210G.github.io/Hydragent/install.ps1 | Out-File install.ps1; .\install.ps1 -Source -Force

.NOTES
    Hydragent installer v1.0.0
#>
[CmdletBinding()]
param(
    [switch]$Source,
    [switch]$SkipOnboard,
    [switch]$Force,
    [string]$Version = 'latest',
    [string]$InstallRoot = "$env:USERPROFILE\.hydragent",
    [string]$Repo = 'joker0210G/Hydragent',
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'

# ===========================================================================
# 0. Console encoding setup (must run BEFORE any Write-Host)
# ===========================================================================
# Box-drawing characters in Write-Banner (and the bullet in the next-steps
# section, etc.) need the console OUTPUT codepage to be 65001 / UTF-8.
# On PowerShell 5.1 running in the legacy Windows conhost, [Console]::OutputEncoding
# alone is not enough: Write-Host re-encodes its output through the
# console OUTPUT codepage, which defaults to the OEM codepage (437,
# 850, etc.). `chcp.com 65001` flips that codepage. We do it at the very
# top of the script -- before Write-Banner or anything else writes a
# single byte -- so every Write-Host in the run flows UTF-8.
# On PowerShell 7+ / Windows Terminal this is a no-op (already UTF-8).
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
try { & chcp.com 65001 > $null 2>&1 } catch { }

# IMPORTANT: this file MUST be saved with a UTF-8 BOM. PowerShell 5.1's
# parser guesses the encoding from the BOM and falls back to the system
# codepage (cp1252) when there is none -- which silently re-decodes the
# UTF-8 box-drawing bytes in Write-Banner as Latin-1 mojibake and then
# chokes on it as if a string was unterminated. The presence of the BOM
# tells the parser "this is UTF-8", so the box-drawing characters are
# decoded correctly and the script parses cleanly. PowerShell 7+
# ignores the BOM entirely, so this is harmless on modern shells.

# ===========================================================================
# 1. Config & shared variables
# ===========================================================================
$BinName      = 'hydragent.exe'
$LauncherName = 'Hydragent.cmd'
$BinDir       = Join-Path $InstallRoot 'bin'
$DataDir      = Join-Path $InstallRoot 'data'
$SourceDir    = Join-Path $InstallRoot 'src'
$LauncherPath = Join-Path $BinDir $LauncherName

# Split $Repo into Org + Name so we can derive all the URLs from it.
# Example: joker0210G/Hydragent  =>  Org=joker0210G, Name=Hydragent
$Org  = ($Repo -split '/')[0]
$Name = ($Repo -split '/')[1]

# Where the hosted install scripts live (GitHub Pages).
# When a custom domain is configured (docs/CNAME) the same URL works
# at e.g. https://hydragent.dev/install.ps1.
$PagesBase    = "https://$Org.github.io/$Name"
$InstallerUrl = "$PagesBase/install.ps1"

# Where prebuilt release zips live.
$ReleaseBase  = "https://github.com/$Repo/releases"
$ApiBase      = "https://api.github.com/repos/$Repo"

$AnsiReset   = "$([char]27)[0m"
$AnsiBold    = "$([char]27)[1m"
$AnsiDim     = "$([char]27)[2m"
$AnsiGreen   = "$([char]27)[32m"
$AnsiYellow  = "$([char]27)[33m"
$AnsiRed     = "$([char]27)[31m"
$AnsiCyan    = "$([char]27)[36m"
$AnsiMagenta = "$([char]27)[35m"

# ===========================================================================
# 1. Helper functions (declared first so they are always visible)
# ===========================================================================

function Write-Banner {
    if ($Quiet) { return }
    # Console encoding (UTF-8 / chcp 65001) is set up at the top of the
    # script, before any Write-Host runs. See section 0 at the top of
    # this file (Console encoding setup).
    Write-Host ''
    Write-Host "$AnsiCyan$AnsiBold██╗  ██╗██╗   ██╗██████╗ ██████╗  █████╗  ██████╗ ███████╗███╗   ██╗████████╗$AnsiReset"
    Write-Host "$AnsiCyan$AnsiBold██║  ██║╚██╗ ██╔╝██╔══██╗██╔══██╗██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝$AnsiReset"
    Write-Host "$AnsiCyan$AnsiBold███████║ ╚████╔╝ ██║  ██║██████╔╝███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   $AnsiReset"
    Write-Host "$AnsiCyan$AnsiBold██╔══██║  ╚██╔╝  ██║  ██║██╔══██╗██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   $AnsiReset"
    Write-Host "$AnsiCyan$AnsiBold██║  ██║   ██║   ██████╔╝██║  ██║██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   $AnsiReset"
    Write-Host "$AnsiCyan$AnsiBold╚═╝  ╚═╝   ╚═╝   ╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝   $AnsiReset"
    Write-Host "$AnsiDim  one-command installer$AnsiReset"
    Write-Host ''
}

function Write-OK    { param($m) Write-Host "$AnsiGreen  ok$AnsiReset  $m" }
function Write-Info  { param($m) Write-Host "$AnsiCyan  ..$AnsiReset  $m" }
function Write-Warn  { param($m) Write-Host "$AnsiYellow  !!$AnsiReset  $m" }
function Write-Err   { param($m) Write-Host "$AnsiRed  ERR$AnsiReset $m" ; exit 1 }
function Write-Step  {
    param($n, $m)
    if ($Quiet) { return }
    Write-Host ''
    Write-Host "$AnsiBold$AnsiMagenta[$n]$AnsiReset $AnsiBold$m$AnsiReset"
}

function Test-Command {
    param($name)
    return $null -ne (Get-Command $name -ErrorAction SilentlyContinue)
}

function Get-TargetTriple {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch.ToString()) {
        'X64'    { return 'x86_64-pc-windows-msvc' }
        'Arm64'  { return 'aarch64-pc-windows-msvc' }
        default  { Write-Err "Unsupported architecture: $arch" }
    }
}

function Ensure-Directory {
    param($path)
    New-Item -ItemType Directory -Force -Path $path | Out-Null
}

# ---------------------------------------------------------------------------
# 1b. Install helpers (used by the Rust installer)
# ---------------------------------------------------------------------------

# Always-writable temp directory owned by Hydragent. Using $env:TEMP is
# fragile on Windows: it can be a short 8.3 path (C:\Users\FOO~1\...),
# a UNC path with embedded credentials, or a directory full of stale
# half-written installer scripts. Remove-Item has been observed to throw
# "object at the specified path X does not exist" on those legacy
# short-name paths during the cleanup step of rustup-init, which made
# the installer loop forever. Using our own directory under the install
# root avoids all of that.
function Get-HydraTempDir {
    $dir = Join-Path $InstallRoot 'tmp'
    Ensure-Directory $dir
    return $dir
}

# Locate an existing cargo.exe, even if not on the current session's
# PATH. Returns the directory containing cargo.exe, or $null.
#
# Checks, in order:
#   1. Current session PATH (Get-Command)
#   2. %USERPROFILE%\.cargo\bin (the rustup default)
#   3. C:\Program Files\Rust\bin (the legacy .msi installer location)
function Find-ExistingCargo {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd -and (Test-Path $cmd.Path)) {
        return Split-Path -Parent $cmd.Path
    }

    $candidates = @(
        (Join-Path $env:USERPROFILE '.cargo\bin'),
        'C:\Program Files\Rust\bin'
    )
    foreach ($c in $candidates) {
        if (Test-Path (Join-Path $c 'cargo.exe')) { return $c }
    }

    return $null
}

# Locate the Visual Studio Build Tools install that ships the MSVC
# toolchain (cl.exe + link.exe + Windows SDK). Returns the install
# root, or $null if not installed. Uses vswhere.exe, which ships with
# every Visual Studio install (Community, Professional, Build Tools).
function Find-MsvcBuildTools {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }

    try {
        $path = & $vswhere -latest -products * `
                       -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                       -property installationPath 2>$null
    } catch {
        return $null
    }
    if ($LASTEXITCODE -ne 0) { return $null }
    if ([string]::IsNullOrWhiteSpace($path)) { return $null }
    return $path
}

# Verify that a cargo install actually works end-to-end. Just checking
# `cargo --version` is necessary but not sufficient: the MSVC toolchain
# can install successfully yet fail every `cargo build` because the
# Visual Studio Build Tools (or "Windows SDK") aren't installed -- this
# is exactly the warning that fires during a bare rustup install:
#
#     warn: installing msvc toolchain without its prerequisites
#
# We catch it by probing for the linker (link.exe) via vswhere when the
# installed triple looks like MSVC.
function Test-CargoToolchain {
    param([string]$CargoBin)

    $cargoExe = Join-Path $CargoBin 'cargo.exe'
    if (-not (Test-Path $cargoExe)) { return $false }

    $env:Path = "$CargoBin;$env:Path"

    $cv = & cargo --version 2>$null
    if ($LASTEXITCODE -ne 0) { return $false }

    $rv = & rustc --version 2>$null
    if ($LASTEXITCODE -ne 0) { return $false }

    if ($cv -match 'msvc') {
        if (-not (Find-MsvcBuildTools)) { return $false }
    }

    return $true
}

# Add a directory to the persistent user PATH (and the current session's
# PATH), if it's not already there. This is the canonical way to expose
# a newly-installed toolchain to FUTURE shells; new shells that the
# user opens will inherit it automatically via the User env var.
function Install-PersistentPathEntry {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Dir
    )

    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = if ($current) { $current -split ';' } else { @() }
    if ($entries -contains $Dir) {
        Write-Info "PATH already contains $Dir"
        $env:Path = "$Dir;$env:Path"
        return
    }
    $new = if ($current) { "$Dir;$current" } else { $Dir }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
    $env:Path = "$Dir;$env:Path"
    Write-OK "Added $Dir to user PATH"
}

# ---------------------------------------------------------------------------
# 2. Install steps
# ---------------------------------------------------------------------------

function Install-FromRelease {
    [CmdletBinding()]
    param([string]$Ver)

    Write-Step 1 'Resolving latest release from GitHub'
    $triple = Get-TargetTriple
    $v = $Ver
    if ($v -eq 'latest') {
        $api = "$ApiBase/releases/latest"
        $rel = Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent' = 'hydragent-installer' }
        $v = $rel.tag_name
        if (-not $v) { throw "Could not resolve 'latest' from $api" }
    }
    Write-Info "Version: $v  ($triple)"

    $asset    = "hydragent-$v-$triple.zip"
    $download = "$ReleaseBase/download/$v/$asset"
    $zipPath  = Join-Path (Get-HydraTempDir) $asset

    Write-Step 2 "Downloading prebuilt binary"
    Write-Info "URL: $download"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $download -OutFile $zipPath
    } catch {
        throw "Download failed (network error or release $v not published): $_"
    }

    Write-Step 3 "Extracting to $BinDir"
    if (Test-Path (Join-Path $BinDir $BinName)) {
        Remove-Item (Join-Path $BinDir $BinName) -Force
    }
    Expand-Archive -Path $zipPath -DestinationPath $BinDir -Force
    try { Remove-Item $zipPath -Force -ErrorAction SilentlyContinue } catch { }

    if (-not (Test-Path (Join-Path $BinDir $BinName))) {
        throw "Extraction succeeded but $BinName not found in archive"
    }
}

function Install-FromSource {
    [CmdletBinding()]
    param(
        [ValidateSet('auto', 'msvc', 'gnu')]
        [string]$Toolchain = 'auto'
    )

    Install-RustIfMissing -Toolchain $Toolchain
    Install-SourceCheckout
    Build-Source
}

function Install-RustIfMissing {
    [CmdletBinding()]
    param(
        [switch]$Force,
        [ValidateSet('auto', 'msvc', 'gnu')]
        [string]$Toolchain = 'auto'
    )

    # 1. Fast path: a working toolchain is already on PATH (or in a
    #    well-known install dir). Confirm it works end-to-end, persist
    #    the PATH entry, then bail.
    if (-not $Force) {
        $existingBin = Find-ExistingCargo
        if ($existingBin -and (Test-CargoToolchain -CargoBin $existingBin)) {
            $cv = (& cargo --version) -replace "`r`n", ''
            $rv = (& rustc --version) -replace "`r`n", ''
            Write-OK "Rust toolchain ready: $rv / $cv  (at $existingBin)"
            Install-PersistentPathEntry -Dir $existingBin | Out-Null
            return
        }
        if ($existingBin) {
            Write-Warn "cargo found at $existingBin but failed sanity check; reinstalling."
        }
    }

    # 2. Decide which Rust target triple to install. Default follows the
    #    OS preference: MSVC if Visual Studio Build Tools are installed
    #    (the canonical Windows target), GNU otherwise. Override with
    #    -Toolchain msvc|gnu.
    $triple = Get-TargetTriple   # x86_64-pc-windows-msvc | aarch64-pc-windows-msvc
    if ($Toolchain -eq 'auto') {
        if (Find-MsvcBuildTools) {
            Write-Info "MSVC build tools detected -> using MSVC toolchain."
            # $triple already ends in -msvc; nothing to change.
        } else {
            Write-Warn "MSVC build tools not detected."
            Write-Warn "Falling back to the GNU toolchain (rustup will install MinGW)."
            Write-Warn "For MSVC later, install Visual Studio Build Tools:"
            Write-Warn "  https://visualstudio.microsoft.com/visual-cpp-build-tools/"
            $triple = $triple -replace '-msvc$', '-gnu'
        }
    } elseif ($Toolchain -eq 'gnu') {
        $triple = $triple -replace '-msvc$', '-gnu'
    }
    # -Toolchain 'msvc' keeps $triple as-is.
    Write-Info "Target triple: $triple"

    # 3. Prepare a safe temp dir. See Get-HydraTempDir for the rationale.
    $work = Get-HydraTempDir
    $tmp  = Join-Path $work 'rustup-init.exe'
    $log  = Join-Path $work 'rustup-init.log'

    # 4. Download rustup-init. Try the canonical URL first, then fall
    #    back to winget if a network proxy blocks the direct download.
    Write-Step 'A1' "Installing Rust toolchain via rustup"
    $url = 'https://win.rustup.rs/x86_64'
    Write-Info "Downloading $url"

    $skipRun = $false
    try {
        if (Test-Path $tmp) { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
        if (Test-Path $log) { Remove-Item $log -Force -ErrorAction SilentlyContinue }
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tmp -ErrorAction Stop
    } catch {
        Write-Warn "Direct rustup download failed: $($_.Exception.Message)"
        if (Test-Command winget) {
            Write-Info "Trying winget as a fallback..."
            try {
                & winget install --id Rustlang.Rustup -e --source winget `
                                 --accept-package-agreements --accept-source-agreements `
                                 --disable-interactivity
                if ($LASTEXITCODE -eq 0) {
                    Write-OK "Rust installed via winget"
                    $skipRun = $true   # winget handled the whole install
                } else {
                    Write-Warn "winget exited with code $LASTEXITCODE"
                }
            } catch {
                Write-Warn "winget failed: $($_.Exception.Message)"
            }
        } else {
            Write-Warn "winget is not available either."
        }
        if (-not $skipRun) {
            Write-Err "Could not download rustup-init.exe. Check network/proxy and retry."
        }
    }

    # 5. Sanity-check the download. A real rustup-init.exe is ~10 MB.
    #    Anything smaller than 1 MB is a redirect or error page, not
    #    the installer. Catch this BEFORE we try to run it.
    if (-not $skipRun) {
        if (-not (Test-Path $tmp)) {
            Write-Err "rustup-init.exe was not created at $tmp"
        }
        $size = (Get-Item $tmp).Length
        if ($size -lt 1MB) {
            Remove-Item $tmp -Force -ErrorAction SilentlyContinue
            Write-Err "rustup-init.exe is only $size bytes; download likely failed."
        }
        Write-Info "Downloaded $([math]::Round($size / 1MB, 1)) MB"

        # 6. Run rustup-init. Stream output so the user sees progress
        #    (a fresh toolchain install is 1-3 minutes) AND we keep a
        #    transcript for bug reports.
        Write-Info "Running rustup-init -y --default-toolchain stable-$triple --profile minimal"
        Write-Info "(this typically takes 1-3 minutes for a fresh install)"
        & $tmp -y --default-toolchain "stable-$triple" --profile minimal --no-modify-path `
            2>&1 | Tee-Object -FilePath $log | Out-Host
        $rustupExit = $LASTEXITCODE
    } else {
        $rustupExit = 0
    }

    # 7. Best-effort cleanup of the temp dir contents (never crash).
    #    The Rust install itself lives in ~/.cargo and ~/.rustup, which
    #    are NOT touched here.
    try {
        if (Test-Path $tmp) { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
        if ($rustupExit -eq 0 -and (Test-Path $log)) {
            # Keep last 100 lines as a debug breadcrumb.
            Get-Content $log -Tail 100 -ErrorAction SilentlyContinue |
                Set-Content -Path "$log.last" -Force -ErrorAction SilentlyContinue
            Remove-Item $log -Force -ErrorAction SilentlyContinue
            if (Test-Path "$log.last") {
                Move-Item "$log.last" $log -Force -ErrorAction SilentlyContinue
            }
        }
    } catch { }

    if ($rustupExit -ne 0) {
        Write-Err "rustup-init exited with code $rustupExit. See $log for details."
    }

    # 8. Verify the new toolchain actually works end-to-end.
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (-not (Test-Path $cargoBin)) {
        Write-Err "Rust installation reported success but $cargoBin does not exist."
    }

    # 9. If we installed MSVC but Build Tools aren't actually present,
    #    the toolchain is broken (rustc will work, cargo build will fail
    #    with a missing linker). Warn loudly so the user knows what to
    #    install next.
    if ($triple.EndsWith('-msvc') -and -not (Find-MsvcBuildTools)) {
        Write-Warn "Rust installed with MSVC target, but Visual Studio Build Tools are missing."
        Write-Warn "`cargo build` will fail with linker errors until you install:"
        Write-Warn "  https://visualstudio.microsoft.com/visual-cpp-build-tools/"
        Write-Warn "Tick the 'Desktop development with C++' workload (includes Windows SDK)."
    }

    if (-not (Test-CargoToolchain -CargoBin $cargoBin)) {
        Write-Err "Rust installation completed but the toolchain is not functional. Check $log."
    }

    # 10. Persist the PATH update so future shells pick up cargo.
    Install-PersistentPathEntry -Dir $cargoBin | Out-Null

    $cv = (& cargo --version) -replace "`r`n", ''
    $rv = (& rustc --version) -replace "`r`n", ''
    Write-OK "Rust installed: $rv / $cv  (toolchain: stable-$triple)"
}

function Install-SourceCheckout {
    $marker = Join-Path $SourceDir 'Cargo.toml'
    if (Test-Path $marker) {
        Write-Info "Source checkout already present at $SourceDir"
        Push-Location $SourceDir
        try {
            if (Test-Command git) {
                & git pull --ff-only 2>$null | Out-Null
            }
        } catch { }
        Pop-Location
        return
    }

    Write-Step 'A2' "Cloning $Repo into $SourceDir"
    if (-not (Test-Command git)) {
        Write-Err "git is required for a from-source install. Install git for Windows and retry, or use the prebuilt binary."
    }
    & git clone "https://github.com/$Repo.git" $SourceDir
    if ($LASTEXITCODE -ne 0) {
        Write-Err "git clone failed (exit $LASTEXITCODE). Check network / repo URL."
    }
}

function Build-Source {
    Write-Step 'A3' "Building hydragent-core (release)"
    Push-Location $SourceDir
    try {
        & cargo build --release -p hydragent-core
        if ($LASTEXITCODE -ne 0) {
            Write-Err "cargo build failed (exit $LASTEXITCODE). See output above."
        }
        $built = Join-Path $SourceDir "target\release\$BinName"
        if (-not (Test-Path $built)) {
            Write-Err "Build reported success but $built not found."
        }
        Copy-Item $built (Join-Path $BinDir $BinName) -Force
        Write-OK "Built and installed $BinName"
    } finally {
        Pop-Location
    }
}

function Install-Launcher {
    Write-Step 4 "Installing $LauncherName launcher"
    $body = @"
@echo off
REM Hydragent launcher (auto-generated by install.ps1)
setlocal EnableExtensions
if "%HYDRAGENT_HOME%"=="" set "HYDRAGENT_HOME=$InstallRoot"
set "HYDRAGENT_BIN=%HYDRAGENT_HOME%\bin"
set "HYDRAGENT_DATA_DIR=$DataDir"
if /I "%~1"=="install" goto :do_install
if /I "%~1"=="update" goto :do_update
if /I "%~1"=="uninstall" goto :do_uninstall
if not exist "%HYDRAGENT_BIN%\hydragent.exe" goto :do_install
if "%~1"=="" (
    rem The canonical .env lives at %HYDRAGENT_HOME%\.env (top-level),
    rem not in the data dir. See crates\hydragent-core\src\paths.rs.
    if exist "%HYDRAGENT_HOME%\.env" ( set "_CMD=serve" ) else ( set "_CMD=onboard" )
    "%HYDRAGENT_BIN%\hydragent.exe" !_CMD!
    exit /b %ERRORLEVEL%
)
"%HYDRAGENT_BIN%\hydragent.exe" %*
exit /b %ERRORLEVEL%
:do_install
powershell -NoProfile -ExecutionPolicy Bypass -File "%HYDRAGENT_BIN%\install.ps1" %*
exit /b %ERRORLEVEL%
:do_update
"%HYDRAGENT_BIN%\hydragent.exe" update %*
exit /b %ERRORLEVEL%
:do_uninstall
"%HYDRAGENT_BIN%\hydragent.exe" uninstall %*
exit /b %ERRORLEVEL%
"@
    Set-Content -Path $LauncherPath -Value $body -Encoding ASCII -Force
    Write-OK "Launcher written: $LauncherPath"
}

function Install-SelfCopy {
    # Copy this very script into $BinDir so the launcher can find it for
    # `Hydragent install` without re-downloading. If the launcher ever
    # points at a missing install.ps1, it falls back to downloading from
    # raw.githubusercontent.com — so this is a perf / offline convenience,
    # not a hard requirement.
    # $PSCommandPath is set when the script is invoked by file path
    # (e.g. `install.ps1 -Foo`). When invoked via `iex` (the one-liner
    # `irm ... | iex`), $PSCommandPath is $null, so we silently skip.
    $scriptPath = $PSCommandPath
    if (-not $scriptPath -or -not (Test-Path $scriptPath)) { return }
    $selfDest = Join-Path $BinDir 'install.ps1'
    try {
        Copy-Item -LiteralPath $scriptPath -Destination $selfDest -Force -ErrorAction Stop
        Write-OK "Installer copied to $selfDest"
    } catch {
        Write-Warn "Could not copy installer to $selfDest ($($_.Exception.Message))."
        Write-Warn "Hydragent install will re-download install.ps1 from the canonical URL."
    }
}

function Install-PathEntry {
    Install-PersistentPathEntry -Dir $BinDir
}

function Invoke-Onboarding {
    Write-Step 5 "Launching first-run onboarding wizard"
    if (-not (Test-Command hydragent)) {
        $env:Path = "$BinDir;$env:Path"
    }
    & (Join-Path $BinDir $BinName) onboard
}

function Write-NextSteps {
    if ($Quiet) { return }
    Write-Host ''
    Write-Host "$AnsiBold  Next steps:$AnsiReset"
    Write-Host ''
    Write-Host "    hydragent status          # one-shot status dashboard"
    Write-Host "    hydragent serve           # start the gateway (foreground)"
    Write-Host "    hydragent ps              # list running gateways"
    Write-Host "    hydragent stop [pid]      # stop a running gateway"
    Write-Host "    hydragent chat            # interactive REPL"
    Write-Host ''
    Write-Host "$AnsiDim  Open a NEW terminal so the updated PATH is picked up.$AnsiReset"
    Write-Host ''
}

<#
Pause the script at the end so a one-liner `irm ... | iex` invocation
does not flash the window closed before the user can read the result.

Detection rule (all conditions must be true to pause):
  - $Quiet is not set
  - Not in a CI environment
  - stdin  is NOT redirected   (no pipe feeding into the script)
  - stdout is NOT redirected   (no pipe capturing the script output)
  - stderr is NOT redirected
  - $Host.UI.RawUI.WindowTitle is non-empty (we have a real conhost window)

Why so defensive? Because [Console]::ReadKey($true) BLOCKS forever on
a piped stdin -- there is no key the user can press, the call just
hangs. So we have to be sure we are in an interactive console before
calling it.

In practice this means the pause fires when the user runs:
    irm https://...install.ps1 | iex
from a fresh conhost window (Start Menu -> Windows PowerShell, or
Win+R -> "powershell"). It does NOT fire when the script is run from
a CI runner, piped from another command, or invoked via `-File` from
within an existing interactive session that itself has output piped.
#>
function Pause-IfEphemeral {
    if ($Quiet) { return }

    # CI runners never have a real console attached, so always skip
    if ($env:CI -or $env:GITHUB_ACTIONS -or $env:BUILD_NUMBER -or
        $env:TF_BUILD   -or $env:JENKINS_URL) {
        return
    }

    # If any of stdin/stdout/stderr are redirected, there is either
    # no input device for the user to press a key on, or no console
    # output device for `Press any key to close this window...` to
    # render meaningfully. Skip in that case.
    try {
        if ([Console]::IsInputRedirected)  { return }
        if ([Console]::IsOutputRedirected) { return }
        if ([Console]::IsErrorRedirected)  { return }
    } catch {
        # If the redirect probes fail for any reason, bail to safe behaviour
        return
    }

    # Finally, confirm we have a real conhost window by checking that
    # RawUI can give us a non-empty window title. This is the strongest
    # signal that we are running inside an actual interactive console.
    try {
        $ui = $Host.UI.RawUI
        if ($null -eq $ui) { return }
        if ([string]::IsNullOrEmpty($ui.WindowTitle)) { return }
    } catch {
        return
    }

    Write-Host ''
    Write-Host "$AnsiDim  Press any key to close this window...$AnsiReset"
    # ReadKey($true) returns without echoing the key. If the user
    # closes the window mid-pause, ReadKey throws -- swallow it.
    try {
        $null = [Console]::ReadKey($true)
    } catch {
        # Best-effort; exit 0 either way.
    }
}

# ===========================================================================
# 3. Main flow
# ===========================================================================

Write-Banner
Write-Info "Install root: $InstallRoot"
Write-Info "Repo:         $Repo"
Write-Info "Version:      $Version"
Write-Info "Mode:         $(if ($Source) { 'source' } else { 'auto (prefer prebuilt)' })"

Ensure-Directory $BinDir
Ensure-Directory $DataDir

$alreadyInstalled = Test-Path (Join-Path $BinDir $BinName)

if ($alreadyInstalled -and -not $Force) {
    $existing = & (Join-Path $BinDir $BinName) --version 2>$null
    if ($existing) {
        Write-OK "Hydragent is already installed: $existing"
    } else {
        Write-OK "Hydragent is already installed at $(Join-Path $BinDir $BinName)"
    }
    Write-Info "Pass -Force to reinstall, run 'hydragent update' to update, or 'hydragent uninstall' to remove."
    # Re-run the PATH/launcher steps in case the user nuked them.
    Install-Launcher
    Install-SelfCopy
    Install-PathEntry
    if (-not $SkipOnboard) { Invoke-Onboarding }
    Write-NextSteps
    Pause-IfEphemeral
    return
}

if ($Source) {
    Install-FromSource
} else {
    try {
        Install-FromRelease -Ver $Version
    } catch {
        Write-Warn "Prebuilt release unavailable: $_"
        Write-Warn "Falling back to building from source (will install Rust if needed)..."
        Install-FromSource
    }
}

Install-Launcher
Install-SelfCopy
Install-PathEntry

Write-OK "Hydragent installed to $BinDir"
Write-OK "Data directory: $DataDir"
Write-OK "Launcher:       $LauncherPath"
Write-OK "PATH updated (new shells will pick this up)"

if (-not $SkipOnboard) { Invoke-Onboarding }
Write-NextSteps
Pause-IfEphemeral
