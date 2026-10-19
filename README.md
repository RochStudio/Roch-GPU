<img src="assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU (Rust)

A Windows tuning and monitoring tool for NVIDIA, AMD and Intel Arc GPUs: supported clocks, voltage, power, fans and memory timings, live telemetry, five profile slots and light/dark modes. No HWiNFO dependency.

This is a Rust rewrite of [Roch GPU 1.1.0](https://github.com/RochStudio/Roch-GPU) (C#/WPF). It keeps the same features, driver handling, CLI, and profile/settings files, so an existing `%AppData%\Roch GPU` folder carries over.

## What changed from the C# build

- **One native ~6 MB executable** instead of a ~70 MB self-contained .NET publish; no runtime to unpack at start.
- **GUI in egui** (eframe, OpenGL) instead of WPF. Same windows: main, V/F Curve, XOC, Fan, Telemetry, Graphics, plus the tray icon.
- **Driver writes run on a worker thread**, so Apply/Reset no longer block the window.
- **The NVAPI layer is reimplemented** directly against `nvapi64.dll` rather than through the vendored NvAPIWrapper library.
- **Full driver ranges.** Core, memory, XBAR, SYS and video offsets offer the whole range the driver reports (e.g. −1000…+1000 MHz core on an RTX 5070 Ti), with both ends reachable; v1.1.0 narrowed them to a "practical" window.
- **`--mock` skips the UAC prompt** (the simulated GPU writes nothing). Real launches of the window still elevate themselves.
- **Software temperature limit** (new; not in v1.1.0). RTX 50 drivers offer no settable temperature limit (no NVAPI thermal policy, and NVML refuses the target temperature), so the hardware Temperature Limit row is hidden there. In its place the window shows **Temperature Limit (°C)**, defaulting to the selected card's own reported limit (its GPU-max temperature; 88 °C on a 5070 Ti) and running on to **105 °C**. Different GPUs therefore get different defaults. At or above the card's own limit the card manages itself and the software does nothing; 105 °C is just the top of the selectable range. Set it below the card's own limit and, while Roch GPU runs (window or tray), it caps the core clock through the same NVML clock lock as the XOC Clock Range whenever the GPU reaches the limit, a 15–45 MHz step a second, and lets the clock climb back once the card is 3 °C below it. It never caps below half the card's maximum clock or the user's own Clock Range floor, never lifts the user's Clock Range ceiling, and hands the clock back (to that range, or to the driver) on exit, Reset or an Apply without it. The card's own slowdown (90 °C on a 5070 Ti) still applies and cannot be raised from user mode; Roch GPU's limit uses the same NVML clock lock, not a hardware thermal write. Saved in profiles as `SoftThermalLimitC` (0 = off/default), which the C# build ignores.

## Install / run

Build it (below), put `RochGPU.exe` in a folder you own and run it. Allow administrator access when asked. Open **Telemetry** to monitor the card, or edit a setting and press **Apply**.

The same executable is the command line:

```powershell
.\RochGPU.exe info            # identity, capabilities, current tuning
.\RochGPU.exe sensors         # one telemetry sample, including extra sensors
.\RochGPU.exe monitor         # live readout
.\RochGPU.exe apply --core 120 --mem 1000 --power 110
.\RochGPU.exe apply --soft-temp 83   # RTX 50: software temperature limit (enforced while the app runs)
.\RochGPU.exe apply-profile "Slot 1"
.\RochGPU.exe list-profiles
.\RochGPU.exe reset
.\RochGPU.exe diag            # raw driver dump for bug reports (also writes roch-gpu-diag.txt)
.\RochGPU.exe startup --status
.\RochGPU.exe info --mock     # any verb against a simulated GPU
.\RochGPU.exe info --mock rtx50   # ...one without a hardware temperature limit
```

## Build

Install Rust (stable, `x86_64-pc-windows-msvc`) and run in PowerShell:

```powershell
.\build.ps1
```

It runs the tests and clippy, then builds `dist\RochGPU.exe`. Startup and window timings can be traced with `ROCH_TRACE=1`; see [`crates/roch-gpu/PERF.md`](crates/roch-gpu/PERF.md). For development, `cargo run -p roch-gpu -- --mock` opens the window against the simulated GPU.

## Layout

| Crate | Contents |
|---|---|
| `crates/roch-core` | Models, the `GpuBackend` trait, `TuningService` (apply ordering, XOC levers, fan curves, the software temperature limit, polling), profile store, recovery journal, startup task, mock GPU |
| `crates/roch-nvidia` | NVAPI + NVML: public and private calls, V/F curve, rails, OCP (legacy and R615+), clock domains, diagnostics |
| `crates/roch-amd` | ADL (Overdrive 8) tuning and ADLX telemetry |
| `crates/roch-intel` | IGCL tuning and telemetry for Arc |
| `crates/roch-gpu` | `RochGPU.exe`: backend detection, CLI, and the egui window |

Each vendor crate loads its driver DLL from System32 only, and builds and tests on its own (`cargo test -p roch-nvidia`).

## Verification status

- **Tests:** 225 unit and integration tests. They cover layouts, buffer parsing, apply ordering, profiles, the recovery journal, the software temperature limit (against a simulated card whose temperature follows its clock), the CLI, the view model and the editors. None of them touch a driver.
- **NVIDIA:** on an RTX 5070 Ti with driver 616.92, read-only output matches the C# 1.1.0 build. That covers `info`, `sensors` and `domains`, and the full `diag` dump is byte-identical apart from live readings.
- **Write paths:** the code that writes to the GPU (apply, reset, fans, V/F curve, XOC levers) is a faithful port, but it has **not yet been validated on hardware** in the Rust build. Test carefully before relying on it. The same goes for the software temperature limit: its controller is tested only against the simulated GPU, and its read side (`info` reports `Soft temp limit: 50..105 °C` on the 5070 Ti, with 88 °C as the card's own limit) is the only part run on the card.
- **AMD and Intel:** ported with their unit tests, but not yet run on an AMD or Arc card.

See the C# repository's notes for the hardware validation history of the original. They are copied in [`docs/`](docs/), including [driver compatibility](docs/r615-compatibility.md), [Intel Arc B60](docs/intel-arc-b60.md) and [profile recovery](docs/profile-recovery.md).

> Overclocking can cause crashes, data loss or hardware damage. Test changes carefully. Controls and sensor readings depend on what your driver exposes.

## Credits

- **Soroush Falahati — [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper):** the NVAPI structure layouts and function IDs used by the NVIDIA backend.
- **dumbie — [RadeonTuner](https://github.com/dumbie/RadeonTuner):** the Overdrive 8 calling-convention reference; no RadeonTuner code is included.
- **AMD [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and [ADLX](https://gpuopen.com/manuals/adlx/)**, **Intel [IGCL](https://github.com/intel/drivers.gpu.control-library)**, **NVIDIA NVAPI and NVML:** the driver interfaces used.

Created by **Roch Studio / [@MateoPCTech](https://x.com/MateoPCTech)**. Licensed GPL-3.0-or-later; see [third-party notices](THIRD-PARTY-NOTICES.md).

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)
