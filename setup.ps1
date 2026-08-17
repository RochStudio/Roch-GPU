$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
Write-Host "=== GpuTuner setup ===" -ForegroundColor Green

# --- Step 1: .NET 8 SDK
$hasSdk = $false
try { $sdks = & dotnet --list-sdks 2>$null; if ($sdks -match '^8\.') { $hasSdk = $true } } catch {}
if (-not $hasSdk) {
    Write-Host "Installing .NET 8 SDK via winget..." -ForegroundColor Yellow
    winget install --id Microsoft.DotNet.SDK.8 -e --accept-source-agreements --accept-package-agreements
    # refresh PATH for this session
    $env:Path = [Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [Environment]::GetEnvironmentVariable("Path","User")
    if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) { $env:Path += ";C:\Program Files\dotnet" }
    & dotnet --list-sdks
} else { Write-Host ".NET 8 SDK already installed." }

# --- Step 2: build + test + publish
Write-Host "`nBuilding..." -ForegroundColor Yellow
dotnet build GpuTuner.sln -c Release
if ($LASTEXITCODE -ne 0) { throw "Build failed - copy the errors above and send them to Claude." }
dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
dotnet publish src/GpuTuner.Cli/GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist
dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist

# --- Step 3: run
Write-Host "`n=== gputuner info ===" -ForegroundColor Green
& .\dist\gputuner-cli.exe info
& .\dist\gputuner-cli.exe info *> "$PSScriptRoot\gputuner-info.txt"
Write-Host "`nLaunching GpuTuner GUI..." -ForegroundColor Green
Start-Process (Join-Path $PSScriptRoot "dist\GpuTuner.exe")
Write-Host "Done. If the build failed, send Claude the text above (also saved: build output is in this window)."
