# ---------------------------------------------------------------------------
# Uninstaller for Hydragent (Windows)
# ---------------------------------------------------------------------------

# ── Self-Relaunch with Bypass ExecutionPolicy if restricted ──────────────────
if ($args -notcontains "--bypassed") {
    $policy = Get-ExecutionPolicy -ErrorAction SilentlyContinue
    if ($policy -eq 'Restricted' -or $policy -eq 'AllSigned') {
        $script = $MyInvocation.MyCommand.Definition
        if ($script -and (Test-Path $script)) {
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script --bypassed
            exit $LASTEXITCODE
        }
    }
}

Write-Host "Stopping any running Hydragent processes..." -ForegroundColor Cyan
Stop-Process -Name hydragent -Force -ErrorAction SilentlyContinue

$InstallDir = "$env:USERPROFILE\.hydragent"
if (Test-Path $InstallDir) {
    Write-Host "Removing installation directory: $InstallDir" -ForegroundColor Yellow
    try {
        Remove-Item -Recurse -Force $InstallDir -ErrorAction Stop
        Write-Host "Successfully removed $InstallDir" -ForegroundColor Green
    } catch {
        Write-Host "Warning: Could not remove installation directory. It might be locked: $_" -ForegroundColor Red
    }
} else {
    Write-Host "Installation directory not found: $InstallDir" -ForegroundColor Gray
}

# ── Clean up cargo bin directory ──────────────────────────────────────────
$CargoBinDir = "$env:USERPROFILE\.cargo\bin"
if (Test-Path $CargoBinDir) {
    $TargetExe = Join-Path $CargoBinDir "hydragent.exe"
    if (Test-Path $TargetExe) {
        Write-Host "Removing binary from cargo directory: $TargetExe" -ForegroundColor Yellow
        try {
            Remove-Item -Force $TargetExe -ErrorAction Stop
            Write-Host "Successfully removed $TargetExe" -ForegroundColor Green
        } catch {
            Write-Host "Warning: Could not remove cargo binary ($TargetExe): $_" -ForegroundColor Red
        }
    }
    $TargetCmd = Join-Path $CargoBinDir "hydragent.cmd"
    if (Test-Path $TargetCmd) {
        Remove-Item -Force $TargetCmd -ErrorAction SilentlyContinue
    }
}


# ── PATH cleanup (case-insensitive, handles trailing slashes and duplicates) ──
Write-Host "Cleaning up PATH environment variable..." -ForegroundColor Cyan
$UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($UserPath) {
    $TargetBin = "$env:USERPROFILE\.hydragent\bin"
    $TargetBinClean = $TargetBin.Trim().TrimEnd('\')
    
    $parts = $UserPath -split ';' | Where-Object { $_.Trim() -ne "" }
    $unique = @()
    foreach ($part in $parts) {
        $clean = $part.Trim().TrimEnd('\')
        # Skip the hydragent bin path
        if ($clean -ieq $TargetBinClean) {
            continue
        }
        # Add to unique list if not already present (deduplicate)
        if ($unique -notcontains $clean) {
            $unique += $clean
        }
    }
    
    $NewPath = $unique -join ';'
    [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
    
    # Also update the current process env path
    $processParts = $env:PATH -split ';' | Where-Object { $_.Trim() -ne "" }
    $processUnique = @()
    foreach ($part in $processParts) {
        $clean = $part.Trim().TrimEnd('\')
        if ($clean -ieq $TargetBinClean) {
            continue
        }
        if ($processUnique -notcontains $clean) {
            $processUnique += $clean
        }
    }
    $env:PATH = $processUnique -join ';'
    
    Write-Host "Successfully removed $TargetBin and cleaned up User PATH." -ForegroundColor Green
}

Write-Host "Hydragent has been successfully uninstalled from your PC!" -ForegroundColor Green
