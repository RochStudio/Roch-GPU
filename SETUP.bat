@echo off
:: GpuTuner one-click setup: installs the .NET 10 SDK (if needed), builds, publishes to dist\, launches.
:: Re-launches itself elevated because winget/UAC and the app itself need admin.
net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Requesting administrator rights...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1"
echo.
pause
