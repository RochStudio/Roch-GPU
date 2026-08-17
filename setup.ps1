$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
Write-Host "=== Roch GPU OC setup ===" -ForegroundColor Green

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
dotnet build roch-gpu-oc-beta.sln -c Release
if ($LASTEXITCODE -ne 0) { throw "Build failed - see the errors above." }

dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
if ($LASTEXITCODE -ne 0) { throw "Tests failed - see the failures above." }

# CLI first, GUI last: both publish into dist\, and on case-insensitive NTFS
# the second publish will clean up the first one's output if the order is reversed.
dotnet publish src/GpuTuner.Cli/GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist
if ($LASTEXITCODE -ne 0) { throw "CLI publish failed - see the errors above." }

dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist
if ($LASTEXITCODE -ne 0) { throw "GUI publish failed - is RochGpuOC.exe still running? Close it (check the tray) and re-run." }

foreach ($exe in "dist\rochoc.exe", "dist\RochGpuOC.exe") {
    if (-not (Test-Path $exe)) { throw "$exe missing after publish - the publish order may have clobbered it." }
}

# --- Step 3: report what was detected, then launch
Write-Host "`n=== rochoc info ===" -ForegroundColor Green
$info = & .\dist\rochoc.exe info
$info | Write-Host
$info | Out-File -FilePath (Join-Path $PSScriptRoot "gputuner-info.txt") -Encoding utf8

Write-Host "`nLaunching Roch GPU OC..." -ForegroundColor Green
Start-Process (Join-Path $PSScriptRoot "dist\RochGpuOC.exe")
Write-Host "Done. CLI: dist\rochoc.exe info   Diagnostics: dist\rochoc.exe diag"
