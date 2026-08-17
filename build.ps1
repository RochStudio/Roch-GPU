# Roch GPU OC - build everything and publish a single-folder GUI + CLI into .\dist
# Requires .NET 8 SDK on Windows: winget install Microsoft.DotNet.SDK.8
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# $ErrorActionPreference does not apply to native commands: dotnet failing sets $LASTEXITCODE and
# nothing else. Without these checks a failed build would sail on and publish the previous binaries,
# reporting success while shipping stale output.
dotnet build roch-gpu-oc-beta.sln -c Release
if ($LASTEXITCODE -ne 0) { throw "Build failed - see the errors above." }

dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
if ($LASTEXITCODE -ne 0) { throw "Tests failed - see the failures above." }

# CLI first, GUI last: on case-insensitive NTFS the two publishes must not clean each other up.
dotnet publish src/GpuTuner.Cli/GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist
if ($LASTEXITCODE -ne 0) { throw "CLI publish failed - see the errors above." }

dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist
if ($LASTEXITCODE -ne 0) { throw "GUI publish failed - is RochGpuOC.exe still running? Close it (check the tray) and re-run." }

foreach ($exe in "dist\rochoc.exe", "dist\RochGpuOC.exe") {
    if (-not (Test-Path $exe)) { throw "$exe missing after publish - the publish order may have clobbered it." }
}

Write-Host "`nDone. Run dist\RochGpuOC.exe (GUI, asks for admin) or dist\rochoc.exe info (CLI)."
