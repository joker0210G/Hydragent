@echo off
rem ---------------------------------------------------------------------------
rem Hydragent Windows Uninstaller Wrapper
rem Bypasses execution policy to run the main uninstall.ps1 script.
rem ---------------------------------------------------------------------------
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1"
