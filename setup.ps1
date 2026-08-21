$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
Write-Host "=== Roch GPU setup ===" -ForegroundColor Green

# --- Step 1: .NET 10 SDK
$hasSdk = $false
try { $sdks = & dotnet --list-sdks 2>$null; if ($sdks -match '^10\.') { $hasSdk = $true } } catch {}
if (-not $hasSdk) {
    Write-Host "Installing the .NET 10 SDK via winget..." -ForegroundColor Yellow
    winget install --id Microsoft.DotNet.SDK.10 -e --accept-source-agreements --accept-package-agreements
    # refresh PATH for this session
    $env:Path = [Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [Environment]::GetEnvironmentVariable("Path","User")
    if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) { $env:Path += ";C:\Program Files\dotnet" }
    & dotnet --list-sdks
} else { Write-Host ".NET 10 SDK already installed." }

# --- Step 2: build + test + publish
Write-Host "`nBuilding..." -ForegroundColor Yellow
dotnet build roch-gpu.sln -c Release
if ($LASTEXITCODE -ne 0) { throw "Build failed - see the errors above." }

dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
if ($LASTEXITCODE -ne 0) { throw "Tests failed - see the failures above." }

dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -o dist
if ($LASTEXITCODE -ne 0) { throw "Publish failed - is 'Roch GPU.exe' still running? Close it (check the tray) and re-run." }
if (-not (Test-Path "dist\Roch GPU.exe")) { throw "'dist\Roch GPU.exe' missing after publish." }

# --- Step 3: report what was detected, then launch
Write-Host "`n=== Roch GPU info ===" -ForegroundColor Green
$info = & ".\dist\Roch GPU.exe" info
$info | Write-Host
$info | Out-File -FilePath (Join-Path $PSScriptRoot "roch-gpu-info.txt") -Encoding utf8

Write-Host "`nLaunching Roch GPU..." -ForegroundColor Green
Start-Process (Join-Path $PSScriptRoot "dist\Roch GPU.exe")
Write-Host "Done."
Write-Host "  CLI:         .\dist\'Roch GPU.exe' info"
Write-Host "  Diagnostics: .\dist\'Roch GPU.exe' diag"
