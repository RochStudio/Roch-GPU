@echo off
:: Dumps everything the NVIDIA driver reports about your card, for debugging.
net session >nul 2>&1
if %errorlevel% neq 0 (
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
cd /d "%~dp0"
dist\rochoc.exe diag > rochoc-diag.txt 2>&1
type rochoc-diag.txt
echo.
echo Saved to rochoc-diag.txt - tell Claude "diag done".
pause
