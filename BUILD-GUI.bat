@echo off
cd /d "%~dp0"
echo Building GUI, log goes to gui-build.log ...
dotnet build src\GpuTuner.App\GpuTuner.App.csproj -c Release > gui-build.log 2>&1
dotnet publish src\GpuTuner.App\GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist >> gui-build.log 2>&1
type gui-build.log | findstr /i "error warning succeeded failed"
echo.
echo Done. Tell Claude to read gui-build.log
pause
