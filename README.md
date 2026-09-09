<img src="assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU

A Windows tuning and monitoring tool for NVIDIA and AMD GPUs. Adjust supported clocks, voltage, power, fans and memory timings, with live telemetry, five profile slots and light/dark modes. No HWiNFO dependency.

## Install

1. Download `RochGPU.exe` from [Releases](https://github.com/RochStudio/Roch-GPU/releases).
2. Put it in a folder you own and run it. Allow administrator access when requested.
3. Open **Telemetry** to monitor the card, or edit a setting and press **Apply**.

Windows x64 and a compatible vendor graphics driver are required. The self-contained executable needs no separate .NET installation.

## Build the latest source

Install the **.NET 10 SDK**, then run in PowerShell:

```powershell
.\build.ps1
```

Open `dist\RochGPU.exe`. The current source version is **1.0.4**; it has not been released yet.

> Overclocking can cause crashes, data loss or hardware damage. Test changes carefully. Controls and sensor readings depend on what your driver exposes.

[Detailed reference](docs/reference.md) · [Native AMD telemetry](docs/native-amd-telemetry.md) · [Profile recovery](docs/profile-recovery.md) · [License](LICENSE)
