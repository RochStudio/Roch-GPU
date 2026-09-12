<img src="assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU

A Windows tuning and monitoring tool for NVIDIA and AMD GPUs. Adjust supported clocks, voltage, power, fans and memory timings, with live telemetry, five profile slots and light/dark modes. No HWiNFO dependency.

## Install

1. Download `RochGPU.exe` from [Releases](https://github.com/RochStudio/Roch-GPU/releases).
2. Put it in a folder you own and run it. Allow administrator access when requested.
3. Open **Telemetry** to monitor the card, or edit a setting and press **Apply**.

Windows x64 and a compatible vendor graphics driver are required. The self-contained executable needs no separate .NET installation.

## Features

- **NVIDIA and AMD in one app:** supported core/memory clocks, voltage controls and power limits, with controls adapted to the detected card.
- **Live telemetry:** temperatures, voltages, clocks, utilization, power, fans, GPU memory and PCIe readings where available. Summary cards and current/minimum/maximum/average columns make changes easy to track.
- **Hardware-read GPU identity:** the main header shows driver and vBIOS versions, VRAM type/vendor, maximum PCIe generation and lane width, and Resizable BAR state. The read-only **Graphics** window adds board, silicon, memory and installed-driver details.
- **Native AMD monitoring:** ADL and ADLX read the installed AMD driver directly—no HWiNFO, shared-memory feed or extra monitoring driver.
- **Fan control:** manual speed and fan curves; AMD Zero RPM lives in the Fan window. NVIDIA software curves require the app to keep running.
- **Five saved profiles:** save tuning setups and choose a startup profile. Startup uses a frozen copy; toggle Startup off/on after saving changes to refresh it.
- **NVIDIA clock and voltage controls:** V/F Curve, XOC and Fan editors sit together above the main controls. Supported XBAR, SYS and video offsets appear directly below Core Offset and apply without separate enable switches; potentially damaging rail, OCP and clock-range controls remain individually gated in XOC.
- **AMD memory timings:** select the modes your driver exposes, alongside voltage offset and memory-clock tuning. Unsupported timing modes are not invented.
- **Light/dark interface and CLI:** grouped tuning controls, a separate telemetry window, diagnostics and command-line monitoring/tuning from the same executable.
- **Social links:** YouTube | X | Discord in the bottom-left footer, matching Roch CPU and Viewer.
- **Profile recovery:** full-profile applies keep a recovery journal and attempt rollback after errors or interrupted writes. There is no Keep/Revert countdown, and recovery is not a stability test.

### RX 9070 XT support

Verified native readings include GPU/hotspot/memory temperatures, core voltage, core/memory clocks, board power, fans, utilization, dedicated/shared GPU memory and PCIe link information. FCLK and SoC rows require driver support; the tested RX 9070 XT does not expose them through the public interfaces. Effective core clock and effective FCLK are not implemented. See [native AMD telemetry](docs/native-amd-telemetry.md).

AMD's memory-clock slider sets the maximum memory clock, with limits supplied by the installed driver. On the tested RX 9070 XT, the driver reports 2650–3000 MHz with a 2650 MHz default. This lower slider limit is not the minimum live memory clock; ranges can differ between cards and drivers.

## Screenshots

| NVIDIA tuning | AMD RX 9070 XT tuning | Telemetry |
|---|---|---|
| <img src="assets/screenshots/main.png" alt="Roch GPU NVIDIA main window" width="230"> | <img src="assets/screenshots/amd-main.png" alt="Roch GPU RX 9070 XT main window" width="230"> | <img src="assets/screenshots/amd-monitor.png" alt="Roch GPU AMD telemetry window" width="330"> |

<img src="assets/screenshots/fan.png" alt="Fan speed and curve controls" width="480">

<img src="assets/screenshots/curve.png" alt="NVIDIA voltage-frequency curve editor" width="840">

The AMD tuning and telemetry screenshots show Roch GPU 1.0.4 on an RX 9070 XT. NVIDIA, fan and V/F editor screenshots are from earlier builds. Features depend on the GPU; displayed settings are examples, not tuning recommendations.

## Build the latest source

Install the **.NET 10 SDK**, then run in PowerShell:

```powershell
.\build.ps1
```

Open `dist\RochGPU.exe`. The current version is **1.0.5**.

> Overclocking can cause crashes, data loss or hardware damage. Test changes carefully. Controls and sensor readings depend on what your driver exposes.

## Credits

- **Soroush Falahati — [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper):** the vendored NVAPI binding, modified and retargeted to .NET 10.
- **dumbie — [RadeonTuner](https://github.com/dumbie/RadeonTuner):** the Overdrive 8 calling-convention reference that helped unblock the AMD backend; no RadeonTuner code is included.
- **AMD [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and [ADLX](https://gpuopen.com/manuals/adlx/):** public driver-interface documentation for AMD tuning and telemetry.
- **NVIDIA NVAPI and NVML:** vendor driver interfaces used for NVIDIA tuning and monitoring.

Created by **Roch Studio / [@MateoPCTech](https://x.com/MateoPCTech)**. Licensed GPL-3.0-or-later; third-party components retain their own licenses. See [third-party notices](THIRD-PARTY-NOTICES.md).

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)

[Detailed reference](docs/reference.md) · [Native AMD telemetry](docs/native-amd-telemetry.md) · [Profile recovery](docs/profile-recovery.md) · [License](LICENSE)
