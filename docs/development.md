<img src="../assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU technical guide

A Windows tuning and monitoring tool for NVIDIA, AMD and Intel Arc GPUs: supported clocks, voltage, power, fans and memory timings, live telemetry, five profile slots and light/dark modes. No HWiNFO dependency.

The main branch contains the Rust application. It replaces the [original C#/WPF 1.1.0 build](https://github.com/RochStudio/Roch-GPU/tree/v1.1.0) and keeps compatible profile/settings files, so an existing `%AppData%\Roch GPU` folder carries over.

## What changed from the C# build

- **One native ~6 MB executable** instead of a ~70 MB self-contained .NET publish; no runtime to unpack at start.
- **GUI in egui** (eframe, OpenGL) instead of WPF. Same windows: main, V/F Curve, XOC, Fan, Telemetry, Graphics, plus the tray icon.
- **Driver writes run on a worker thread**, so Apply/Reset no longer block the window.
- **The NVAPI layer is reimplemented** directly against `nvapi64.dll` rather than through the vendored NvAPIWrapper library.
- **Full driver ranges.** Core, memory, XBAR, SYS and video offsets offer the whole range the driver reports (e.g. −1000…+1000 MHz core on an RTX 5070 Ti), with both ends reachable; v1.1.0 narrowed them to a "practical" window.
- **`--mock` skips the UAC prompt** (the simulated GPU writes nothing). Real launches of the window still elevate themselves.
- **Software temperature limit** (new; not in v1.1.0). RTX 50 drivers offer no settable temperature limit (no NVAPI thermal policy, and NVML refuses the target temperature), so the hardware Temperature Limit row is hidden there. In its place the window shows **Temperature Limit (°C)**, defaulting to the selected card's own reported limit (its GPU-max temperature; 88 °C on a 5070 Ti) and running on to **105 °C**. Different GPUs therefore get different defaults. At or above the card's own limit the card manages itself and the software does nothing; 105 °C is just the top of the selectable range. Set it below the card's own limit and, while Roch GPU runs (window or tray), it caps the core clock through the same NVML clock lock as the XOC Clock Range whenever the GPU reaches the limit, a 15–45 MHz step a second, and lets the clock climb back once the card is 3 °C below it. It never caps below half the card's maximum clock or the user's own Clock Range floor, never lifts the user's Clock Range ceiling, and hands the clock back (to that range, or to the driver) on exit, Reset or an Apply without it. The card's own slowdown (90 °C on a 5070 Ti) still applies and cannot be raised from user mode; Roch GPU's limit uses the same NVML clock lock, not a hardware thermal write. Saved in profiles as `SoftThermalLimitC` (0 = off/default), which the C# build ignores.
- **Multiple GPUs** (new). When the active backend reports more than one card, the header's GPU menu switches the whole window — editor, profiles, telemetry, fan/curve windows and the CLI-equivalent state — to another card of that vendor. The choice is saved and restored at the next launch (falling back to the first card if it is gone). Before switching, a software fan curve and temperature limit are handed back to the card they were started on, and each card's recovery journal is opened separately. Same-vendor multi-GPU only: the app still binds to the first backend that initialises, so an NVIDIA + Intel machine shows the NVIDIA card, not both.

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
.\RochGPU.exe info --mock multi   # ...a two-card machine (pick one in the window's GPU menu)
.\RochGPU.exe info --gpu 1    # the second card of the same vendor
```

## Customize the UI

Click the **gear** or press **Ctrl+,**. Changes apply live and save automatically.

- Light, dark or Windows theme; seven colour presets and custom colours.
- Installed fonts, text size, spacing, corner roundness and UI scale.
- Show, hide and reorder panels; choose Telemetry cards, labels, columns and sensors.
- Remember window positions, keep windows on top, adjust opacity and refresh rate.

**Reset all** resets the appearance while keeping GPU tuning and profiles. The bottom bar also opens the settings folder and closes Settings.

## Build

Install Rust (stable, `x86_64-pc-windows-msvc`) and run:

```console
cargo xtask build
```

The Rust build tool checks formatting, runs release tests and clippy, builds and smoke-tests a private copy, then publishes `dist\RochGPU.exe`. Failed checks preserve the existing executable. Use `--out-dir PATH` to build elsewhere while the tool is open. Native Rust AMD diagnostic commands are documented in [`tools/amd`](../tools/amd/README.md).

Startup and window timings can be traced with `ROCH_TRACE=1`; see [`crates/roch-gpu/PERF.md`](../crates/roch-gpu/PERF.md). For development, `cargo run -p roch-gpu -- --mock` opens the window against the simulated GPU.

## Layout

| Crate | Contents |
|---|---|
| `crates/roch-core` | Models, the `GpuBackend` trait, `TuningService` (apply ordering, XOC levers, fan curves, the software temperature limit, polling), profile store, recovery journal, startup task, mock GPU |
| `crates/roch-nvidia` | NVAPI + NVML: public and private calls, V/F curve, rails, OCP (legacy and R615+), clock domains, diagnostics |
| `crates/roch-amd` | ADL (Overdrive 8) tuning and ADLX telemetry |
| `crates/roch-intel` | IGCL tuning and telemetry for Arc |
| `crates/roch-gpu` | `RochGPU.exe`: backend detection, CLI, and the egui window |
| `xtask` | Native Rust build, packaging and AMD diagnostic tools |

Each vendor crate loads its driver DLL from System32 only, and builds and tests on its own (`cargo test -p roch-nvidia`).

## Verification status

- **Tests:** 280 unit and integration tests. They cover layouts, buffer parsing, apply ordering, profiles, UI preferences, the recovery journal, the software temperature limit (against a simulated card whose temperature follows its clock), streaming PMLog sessions, the CLI, the view model and the editors. None of them touch a driver. Two optional audit tests are ignored.
- **NVIDIA:** on an RTX 5070 Ti with driver 616.92, read-only output matches the C# 1.1.0 build. That covers `info`, `sensors` and `domains`, and the full `diag` dump is byte-identical apart from live readings.
- **Write paths:** the code that writes to the GPU (apply, reset, fans, V/F curve, XOC levers) is a faithful port, but it has **not yet been validated on hardware** in the Rust build. Test carefully before relying on it. The same goes for the software temperature limit: its controller is tested only against the simulated GPU, and its read side (`info` reports `Soft temp limit: 50..105 °C` on the 5070 Ti, with 88 °C as the card's own limit) is the only part run on the card.
- **Intel:** on an Arc Pro B60 with driver 32.0.101.9030, `info` and `sensors` read the card correctly, and every write path passed a write/readback/restore run: core offset, memory speed, power, temperature and voltage limits, a V/F point, fixed fan duty, a fan curve and the clock range, with the full tuning state back to its snapshot after each step (`cargo run -p roch-intel --example intel_validate`; see [Intel Arc B60](intel-arc-b60.md)). That covers single-step changes, not stability under load.
- **AMD:** validated on two RX 7900 XTs (Adrenalin 26.10.2, ADL 26.20.15.12). On the display-attached card, `info`, `sensors`, `monitor` and `diag` read correctly, and the write path held up: all four OD8 core-clock write shapes wrote, read back and restored the original snapshot (`cargo xtask amd write-check --apply --offset 2700`), and `apply --core`, `apply --fan` and `reset` each applied and reverted through the CLI. Note `--offset` is an *absolute* FMAX on RDNA 3, not a delta, so it must fall inside the driver's 500..5000 MHz range. A fixed fan duty lands on the 5-point curve because RDNA 3 locks `FanMinSpeed`, `FanAcousticLimit` and `FanTargetTemp` — and stays inaudible at idle while `FanZeroRpmControl` is on, which only the window can toggle. Still unvalidated on hardware: memory clock, power limit, voltage offset, memory timing and stability under sustained load.
- **AMD, a second card with no display attached:** such an adapter first answers with the power-gauge bit alone (`0x10000000`) and a zeroed range table, on all five of its ADL adapter indices — but that is not the driver refusing to tune it. Open a PMLog session on the adapter and the driver hands over the whole of Overdrive 8: capability mask `0x10433F0D` and the same ranges as a card driving a monitor. Both were measured on a second RX 7900 XT, and the mask drops back to `0x10000000` the moment nothing is streaming it. So the backend opens a session and re-reads the table during enumeration whenever a card reports nothing tunable, which is also what its telemetry needs: `ADL2_New_QueryPMLogData_Get` returns an empty buffer for such an adapter while it streams 25 sensors ([`crates/roch-amd/src/pmlog.rs`](../crates/roch-amd/src/pmlog.rs)). ADL enumerates five adapter indices per physical card (one per display output); the backend dedupes them by PCI bus/device/function.
- **Writes land on a display-less card too**, validated on the headless RX 7900 XT: `apply --gpu 1 --power -5` read back as `PowerPercentage` −5 and restored to 0, and a core-clock write read back at 2850 MHz. **But mind its baseline.** Until something has written real values to such a card, the driver reports *floor* values as its current state — `GfxClkFMax` 500 MHz where the display-attached card of the same SKU reports 2850, and `OdVoltage` 700 mV against its own 1100 mV default. Those are not the card's real settings, and because `apply` writes the whole profile (`read_current_as_profile` then the typed flags over it), applying anything on such a card writes the floors back as if they were chosen: a power-limit change alone would clamp the card to 500 MHz and 700 mV. Writing the stock values once (`apply --core 2850 --uv 1100`) makes the driver report them correctly from then on, defaults included. `reset` is the sharp edge — it writes the reported defaults, which on an untouched headless card are those same floors.

See the C# repository's notes for the hardware validation history of the original. They are copied in [`docs/`](./), including [driver compatibility](r615-compatibility.md), [Intel Arc B60](intel-arc-b60.md) and [profile recovery](profile-recovery.md).

> Overclocking can cause crashes, data loss or hardware damage. Test changes carefully. Controls and sensor readings depend on what your driver exposes.

## Credits

- **Soroush Falahati — [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper):** the NVAPI structure layouts and function IDs used by the NVIDIA backend.
- **dumbie — [RadeonTuner](https://github.com/dumbie/RadeonTuner):** the Overdrive 8 calling-convention reference; no RadeonTuner code is included.
- **AMD [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and [ADLX](https://gpuopen.com/manuals/adlx/)**, **Intel [IGCL](https://github.com/intel/drivers.gpu.control-library)**, **NVIDIA NVAPI and NVML:** the driver interfaces used.

Created by **Roch Studio / [@MateoPCTech](https://x.com/MateoPCTech)**. Licensed GPL-3.0-or-later; see [third-party notices](../THIRD-PARTY-NOTICES.md).

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)
