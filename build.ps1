# Roch GPU - test, lint and build the single executable into .\dist
# Requires the Rust toolchain (x86_64-pc-windows-msvc): https://rustup.rs
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# $ErrorActionPreference does not apply to native commands: a failing cargo only sets
# $LASTEXITCODE. Without these checks a failed build would publish the previous binary.
cargo test --workspace --release
if ($LASTEXITCODE -ne 0) { throw "Tests failed - see the failures above." }

cargo clippy --workspace --all-targets --release -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "Clippy reported problems - see above." }

cargo build --release -p roch-gpu
if ($LASTEXITCODE -ne 0) { throw "Build failed - is 'RochGPU.exe' still running? Close it (check the tray) and re-run." }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\RochGPU.exe dist\RochGPU.exe -Force
if (-not (Test-Path "dist\RochGPU.exe")) { throw "'dist\RochGPU.exe' missing after build." }
$mb = [math]::Round((Get-Item "dist\RochGPU.exe").Length / 1MB, 1)

Write-Host "`nDone. dist\RochGPU.exe ($mb MB)"
Write-Host "  GUI:  .\dist\RochGPU.exe"
Write-Host "  CLI:  .\dist\RochGPU.exe info"
