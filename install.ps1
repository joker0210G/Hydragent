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

# NOTE: This file is intentionally saved WITHOUT a UTF-8 BOM. A BOM at
# the start of a script causes `irm ... | iex` (the one-liner install)
# to fail with parser errors because PowerShell's Invoke-Expression
# tokenizer mishandles the BOM-prefixed string when it starts with a
# `<#` block comment. The `irm` cmdlet already receives the raw bytes
# and decodes them; `iex` then evaluates the decoded string -- no BOM
# involvement is needed for this path. The UTF-8 box-drawing characters
# in Write-Banner rely on the console codepage being 65001 (set above
# with chcp.com), not on a file-encoding BOM.

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
    $zipPath  = Join-Path $env:TEMP $asset

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
    Remove-Item $zipPath -Force

    if (-not (Test-Path (Join-Path $BinDir $BinName))) {
        throw "Extraction succeeded but $BinName not found in archive"
    }
}

function Install-FromSource {
    [CmdletBinding()]
    param()

    Install-RustIfMissing
    Install-SourceCheckout
    Build-Source
}

function Install-RustIfMissing {
    if (Test-Command cargo) {
        $cv = (& cargo --version) -replace "`r`n", ''
        Write-OK "Rust already installed: $cv"
        return
    }

    Write-Step 'A1' "Installing Rust toolchain via rustup"
    $url = 'https://win.rustup.rs/x86_64'
    $tmp = Join-Path $env:TEMP 'rustup-init.exe'
    Write-Info "Downloading $url"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tmp
    Write-Info "Running rustup-init -y (stable, minimal profile)"
    & $tmp -y --default-toolchain stable --profile minimal --no-modify-path | Out-Null
    Remove-Item $tmp -Force
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
    if (-not (Test-Command cargo)) {
        Write-Err "Rust installation reported success but cargo is not on PATH. Restart your shell and retry."
    }
    Write-OK "Rust installed: $((& cargo --version) -replace "`r`n", '')"
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
        # Give cargo a generous HTTP timeout (120s instead of 30s) so
        # slow networks or transient crates.io stalls don't kill the
        # install. This is a one-time build; waiting an extra minute
        # is preferable to failing and forcing the user to retry.
        $env:CARGO_HTTP_TIMEOUT = '120'
        & cargo build --release -p hydragent-core
        if ($LASTEXITCODE -ne 0) {
            Write-Err "cargo build failed (exit $LASTEXITCODE). See output above."
        }
        $built = Join-Path $SourceDir "target\release\$BinName"
        if (-not (Test-Path $built)) {
            Write-Err "Build reported success but $built not found."
        }
        $dest = Join-Path $BinDir $BinName
        # On Windows the running .exe is locked, so we can't overwrite
        # it directly. We rename the old binary to .old first, then
        # copy the new one into place. This mirrors the behaviour of
        # hydragent-core/src/update.rs::replace_binary.
        if (Test-Path $dest) {
            $oldDest = $dest + ".old"
            Remove-Item $oldDest -Force -ErrorAction SilentlyContinue
            Rename-Item $dest $oldDest -Force
        }
        Copy-Item $built $dest -Force
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
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = if ($current) { $current -split ';' } else { @() }
    if ($entries -contains $BinDir) {
        Write-Info "PATH already contains $BinDir"
        return
    }
    $new = if ($current) { "$BinDir;$current" } else { $BinDir }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
    $env:Path = "$BinDir;$env:Path"
    Write-OK "Added $BinDir to user PATH"
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
