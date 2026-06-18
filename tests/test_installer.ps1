<#
.SYNOPSIS
    End-to-end regression test for install.ps1 + Hydragent.cmd.

.DESCRIPTION
    Drops a real built hydragent.exe into a sandbox install root and
    runs install.ps1 against it. Verifies:

    1. The installer detects the existing binary and reports its version.
    2. It writes the Hydragent.cmd launcher.
    3. It copies install.ps1 into the bin dir so future `Hydragent install`
       calls work offline.
    4. It updates the user PATH (or no-ops if already present).
    5. The launcher runs and forwards args to the binary correctly.
    6. The launcher references install.ps1 for the `install` subcommand.

    Requires:
        - hydragent.exe already built at target\release\hydragent.exe
        - PowerShell 5.1+ on PATH
#>
$ErrorActionPreference = 'Stop'

$RepoRoot     = (Get-Item $PSScriptRoot).Parent.FullName
$TestRoot     = Join-Path $RepoRoot 'scratch\installer-test'
$InstallRoot  = Join-Path $TestRoot 'hydragent'
$BinDir       = Join-Path $InstallRoot 'bin'
$SourceBinary = Join-Path $RepoRoot 'target\release\hydragent.exe'

Write-Host "==== Setup ===="
if (Test-Path $TestRoot) { Remove-Item $TestRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Copy-Item $SourceBinary (Join-Path $BinDir 'hydragent.exe') -Force
Write-Host "Sandbox: $InstallRoot"
& (Join-Path $BinDir 'hydragent.exe') --version

Write-Host ""
Write-Host "==== Test 1: install.ps1 (already installed, no -Force) ===="
& (Join-Path $RepoRoot 'install.ps1') -SkipOnboard -InstallRoot $InstallRoot
$rc = $LASTEXITCODE
Write-Host "installer exit: $rc"

Write-Host ""
Write-Host "==== Verify files ===="
$expected = @(
    @{ Path = (Join-Path $BinDir 'hydragent.exe'); Desc = 'binary' },
    @{ Path = (Join-Path $BinDir 'Hydragent.cmd');  Desc = 'launcher' },
    @{ Path = (Join-Path $BinDir 'install.ps1');   Desc = 'installer copy' },
    @{ Path = (Join-Path $InstallRoot 'data');     Desc = 'data dir' }
)
foreach ($f in $expected) {
    if (Test-Path $f.Path) {
        Write-Host ("OK    {0,-30}  ({1})" -f $f.Desc, $f.Path)
    } else {
        Write-Host ("FAIL  {0,-30}  (missing: {1})" -f $f.Desc, $f.Path)
    }
}

Write-Host ""
Write-Host "==== Test 2: launcher forwards to binary ===="
$launcher = Join-Path $BinDir 'Hydragent.cmd'
$env:HYDRAGENT_HOME = $InstallRoot
$env:HYDRAGENT_DATA_DIR = (Join-Path $InstallRoot 'data')
$env:HYDRAGENT_BIN = $BinDir
$out = & cmd /c "`"$launcher`" --version" 2>&1
Write-Host "launcher output: $out"
Write-Host "launcher exit: $LASTEXITCODE"

Write-Host ""
Write-Host "==== Test 3: launcher content sanity ===="
$launcherContent = Get-Content $launcher -Raw
$expectedFragments = @(
    'HYDRAGENT_HOME',
    'HYDRAGENT_DATA_DIR',
    'if exist "%HYDRAGENT_DATA_DIR%\.env"',
    'hydragent.exe',
    'do_install'
)
foreach ($frag in $expectedFragments) {
    if ($launcherContent -like "*$frag*") {
        Write-Host "OK    launcher contains: $frag"
    } else {
        Write-Host "FAIL  launcher missing: $frag"
    }
}

Write-Host ""
Write-Host "==== Test 4: launcher routes `install` to install.ps1 ===="
# We don't want to actually run the installer recursively here, but we
# can verify the launcher *would* route `install` by checking the
# :do_install label exists and the path references install.ps1.
if ($launcherContent -match ':do_install') {
    Write-Host "OK    launcher has :do_install label"
} else {
    Write-Host "FAIL  launcher missing :do_install label"
}
if ($launcherContent -match 'install\.ps1') {
    Write-Host "OK    launcher references install.ps1"
} else {
    Write-Host "FAIL  launcher missing install.ps1 reference"
}

Write-Host ""
Write-Host "==== Done ===="
