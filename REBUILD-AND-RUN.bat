@echo off
cd /d "%~dp0"
echo Cleaning build caches and dist, republishing CLI then GUI (log: rebuild.log)...
rmdir /s /q dist 2>nul
rmdir /s /q src\GpuTuner.Cli\obj 2>nul
rmdir /s /q src\GpuTuner.Cli\bin 2>nul
rmdir /s /q src\GpuTuner.App\obj 2>nul
rmdir /s /q src\GpuTuner.App\bin 2>nul
dotnet publish src\GpuTuner.Cli\GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist > rebuild.log 2>&1
dotnet publish src\GpuTuner.App\GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist >> rebuild.log 2>&1
findstr /i "error failed" rebuild.log
dir /b dist\*.exe
echo.
if exist dist\RochGpuOC.exe (
  echo Launching Roch GPU OC - approve the admin prompt...
  start "" "%~dp0dist\RochGpuOC.exe"
) else (
  echo RochGpuOC.exe still missing - send rebuild.log to Claude.
  pause
)
timeout /t 3 >nul
