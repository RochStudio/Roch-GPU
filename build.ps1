# Roch GPU OC - build everything and publish a single-folder GUI + CLI into .\dist
# Requires .NET 8 SDK on Windows: winget install Microsoft.DotNet.SDK.8
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
dotnet build roch-gpu-oc-beta.sln -c Release
dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
# CLI first, GUI last: on case-insensitive NTFS the two publishes must not clean each other up.
dotnet publish src/GpuTuner.Cli/GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist
dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist
Write-Host "`nDone. Run dist\RochGpuOC.exe (GUI, asks for admin) or dist\rochoc.exe info (CLI)."
