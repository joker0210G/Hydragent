# ---------------------------------------------------------------------------
# Uninstaller for Hydragent (Windows)
# ---------------------------------------------------------------------------

$script:exitCode = 0

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

# Give Windows a moment to release file locks after the process exits.
# On Windows, killing a process does not immediately free handles on the exe;
# without this pause Remove-Item can race the kernel cleanup and fail.
Start-Sleep -Milliseconds 500

$InstallDir = "$env:USERPROFILE\.hydragent"
if (Test-Path $InstallDir) {
    $completely = $false
    if ($args -contains "--completely" -or $args -contains "-c") {
        $completely = $true
    } elseif ($args -notcontains "--yes" -and $args -notcontains "-y") {
        Write-Host "How would you like to uninstall Hydragent?" -ForegroundColor Cyan
        Write-Host "  1. Delete ONLY the build/binaries (preserves your memory database, graphs, config .env, and vault)"
        Write-Host "  2. Delete ENTIRELY (deletes all config, databases, memory, and vault)"
        $choice = Read-Host "Select option [1 or 2, default: 1]"
        if ($choice -eq "2") {
            $completely = $true
        } elseif ($choice -ne "1" -and $choice -ne "") {
            Write-Host "Uninstall cancelled." -ForegroundColor Yellow
            exit 0
        }
    }

    if ($completely) {
        Write-Host "Removing installation directory entirely: $InstallDir" -ForegroundColor Yellow
        $removed = $false
        for ($i = 1; $i -le 5; $i++) {
            try {
                Remove-Item -Recurse -Force $InstallDir -ErrorAction Stop
                Write-Host "Successfully removed $InstallDir" -ForegroundColor Green
                $removed = $true
                break
            } catch {
                if ($i -lt 5) {
                    Write-Host "Waiting for file locks to release (attempt $i/5)..." -ForegroundColor Yellow
                    Start-Sleep -Milliseconds (500 * $i)
                } else {
                    Write-Host "Warning: Could not remove installation directory after $i attempts." -ForegroundColor Red
                    Write-Host "  Error: $_" -ForegroundColor Red
                    Write-Host "  Please close any running 'hydragent' processes and run the uninstaller again." -ForegroundColor Red
                    $script:exitCode = 1
                }
            }
        }
    } else {
        Write-Host "Removing ONLY binaries and source directories (preserving data, config, and vault)..." -ForegroundColor Yellow
        $binDir = Join-Path $InstallDir "bin"
        $srcDir = Join-Path $InstallDir "src"
        if (Test-Path $binDir) { Remove-Item -Recurse -Force $binDir -ErrorAction SilentlyContinue }
        if (Test-Path $srcDir) { Remove-Item -Recurse -Force $srcDir -ErrorAction SilentlyContinue }
        Write-Host "Successfully removed binaries and source. Data and config preserved." -ForegroundColor Green
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

if ($script:exitCode -eq 0) {
    Write-Host "Hydragent has been successfully uninstalled from your PC!" -ForegroundColor Green
} else {
    Write-Host "Hydragent uninstall completed with errors. Some files may remain." -ForegroundColor Red
}

exit $script:exitCode
