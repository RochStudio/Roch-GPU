# Roch GPU - build, test, and publish the single executable into .\dist
# Requires the .NET 10 SDK on Windows: winget install Microsoft.DotNet.SDK.10
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# $ErrorActionPreference does not apply to native commands: dotnet failing sets $LASTEXITCODE and
# nothing else. Without these checks a failed build would sail on and publish the previous binary,
# reporting success while shipping stale output.
dotnet build roch-gpu.sln -c Release
if ($LASTEXITCODE -ne 0) { throw "Build failed - see the errors above." }

dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
if ($LASTEXITCODE -ne 0) { throw "Tests failed - see the failures above." }

# One publish, one file. The window and the command line are the same executable: run it with a verb
# for the CLI, with nothing for the GUI. Self-contained, so no runtime has to be installed.
dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -o dist
if ($LASTEXITCODE -ne 0) { throw "Publish failed - is 'Roch GPU.exe' still running? Close it (check the tray) and re-run." }

if (-not (Test-Path "dist\Roch GPU.exe")) { throw "'dist\Roch GPU.exe' missing after publish." }
$mb = [math]::Round((Get-Item "dist\Roch GPU.exe").Length / 1MB, 1)

Write-Host "`nDone. dist\Roch GPU.exe ($mb MB)"
Write-Host "  GUI:  .\dist\'Roch GPU.exe'"
Write-Host "  CLI:  .\dist\'Roch GPU.exe' info"
