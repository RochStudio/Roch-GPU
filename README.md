<img src="assets/logo.svg" alt="Roch GPU OC" width="560">

# Roch GPU OC — beta

[![CI](https://github.com/RochStudio/roch-gpu-oc-beta/actions/workflows/ci.yml/badge.svg)](https://github.com/RochStudio/roch-gpu-oc-beta/actions/workflows/ci.yml)

An Afterburner-style GPU tuning tool for Windows that drives **both NVIDIA and AMD** cards from one
binary. Clocks, voltage, power limit, fan control, live monitoring, a V/F curve editor, and five
profile slots.

No kernel driver — everything goes through the vendors' own user-mode libraries (`nvapi64.dll`,
`atiadlxx.dll`), the same route Afterburner and Adrenalin take.

> **Beta.** Read the [warning](#a-word-of-warning) before using it.

---

## Screenshots

| Main window | Extreme OC (XOC) | Hardware monitor |
|---|---|---|
| <img src="assets/screenshots/main.png" alt="Main window" width="230"> | <img src="assets/screenshots/xoc.png" alt="Extreme OC window" width="250"> | <img src="assets/screenshots/monitor.png" alt="Hardware monitor" width="290"> |

<img src="assets/screenshots/curve.png" alt="V/F curve editor" width="840">

---

## Tested GPUs

The tool asks the driver what it supports rather than assuming, so other cards should work. These
are the ones actually run against hardware:

| GPU | Driver | Status |
|---|---|---|
| **RTX 5070 Ti** (Blackwell) | 610.88 | Full feature set, including the V/F curve editor and everything behind XOC — NVVDD and MSVDD rails, and a crossbar offset that lands and reads back. |
| **RTX 4070 Ti** (Ada) | 591.86 | Everything except the crossbar *write*: the domain is present and measurable, but the driver refuses any non-zero offset. See [Known limitations](#known-limitations). |
| **RTX 4070** (Ada) | — | Expected to behave as the 4070 Ti. Not yet run against hardware. |
| **RX 9070 XT** (RDNA 4) | Adrenalin 25.x–26.x | The AMD feature set: clocks, undervolt offset, power limit, zero RPM, memory timing, hardware fan curve. No editable V/F curve or temperature limit on RDNA 4. |

---

## What you get

The UI is built from what the driver reports, so the controls change with the card rather than
showing everything greyed out.

| Control | NVIDIA | AMD (RDNA+) |
|---|---|---|
| Core clock | offset, 15 MHz steps | offset, MHz |
| Memory clock | offset, 25 MHz steps | absolute clock |
| Voltage boost | %, raises the ceiling | — |
| Voltage cap | in the curve editor's flatten | offset, mV (undervolt) |
| NVVDD / MSVDD rails | floor and ceiling, mV | — |
| XBAR clock | offset, MHz | — |
| Power limit | % of TDP | % offset |
| Temperature limit | ✓ | driver-owned, hidden |
| Fan | fixed %, or a software curve | fixed %, or a hardware curve |
| Zero RPM / memory timing | — | ✓ |
| V/F curve editor | ✓ | no editable curve on RDNA 4 |

Plus a hardware monitor in its own window, five profile slots, apply-at-logon, tray operation and a
CLI.

Offsets snap to the driver's own granularity, so the number on the slider is the number that reaches
the card. The sliders are also narrowed to a range worth dragging — **−150 to +495 MHz** on core and
crossbar, against the ±1000 the driver reports. That ±1000 is the width of the driver's delta field,
not a claim about the silicon.

### V/F curve editor

Reads the card's real table (103 points to 1090 mV on a 4070 Ti, 127 to 1240 mV on a 5070 Ti) and
plots what the card will actually do rather than what is stored.

Drag a point, or select one and use ↑/↓ to nudge it by a 5 MHz step (Shift = 25, Ctrl also flattens
everything above it). Double-click resets a point, right-click resets all. **Flatten above N mV** is
the undervolt, and is where the voltage cap lives.

A marker shows where the card actually stops — the table describes voltages well above anything a
given card selects, so the unreachable stretch is shaded rather than left looking tunable.

### Extreme OC (XOC)

The **XOC** button holds the levers that can brown a card out rather than merely fail: the NVVDD and
MSVDD rail ranges, and the crossbar clock. They sit behind an **Enable / Disable** pair and are off
by default.

Enable and Disable write the card immediately and touch nothing else — no clocks, power or fan. The
gate travels with the profile, so a normal **Apply** respects it: armed writes your values, disarmed
puts the rails and crossbar back to the driver's own. A rail ceiling left standing from an earlier
session is exactly what browns a card out on the next boot, and rail offsets survive a reboot, so
each default is recorded the first time a GPU is seen and restored from there.

---

## Build and run

No prebuilt release yet. You need **Windows 10/11 x64**, the [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
(`winget install Microsoft.DotNet.SDK.8`) and your existing GPU driver.

```powershell
git clone https://github.com/RochStudio/roch-gpu-oc-beta.git
cd roch-gpu-oc-beta
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

That builds, tests and publishes both executables into `dist\`. Then, from an **elevated** terminal:

```powershell
.\dist\RochGpuOC.exe
```

Writing clocks needs administrator rights; building does not. If you don't have the SDK, `SETUP.bat`
does the lot in one double-click.

**No GPU?** Add `--mock` to any CLI command to run against a simulated card.

**Tests:** `dotnet run --project tests/GpuTuner.Core.Tests -c Release` → `215 passed, 0 failed`. The
runner is dependency-free, so most of the engine can be changed without a GPU in front of you.

<details>
<summary>If the build fails</summary>

| Symptom | Cause |
|---|---|
| `does not support targeting .NET 8.0` | SDK older than 8.0 — check `dotnet --list-sdks`. |
| `RochGpuOC.exe` missing from `dist\` | Publishing by hand in the wrong order. The CLI must publish **before** the GUI; both share `dist\` and the second publish cleans the first. |
| `...\dist\... is denied` | The GUI is still running, tray icon included. |
| Builds fine, writes silently do nothing | Not elevated. |

`.\dist\rochoc.exe diag` prints exactly what the driver reported, which is usually the answer.

</details>

---

## Command line

`rochoc.exe` is the same engine without the window.

```
rochoc info                          what was detected, and every limit the driver reports
rochoc monitor [--interval 1000]     live telemetry until Ctrl+C
rochoc apply --core 120 --mem 800 --power 110 --fan 60
rochoc apply --volt 25 --uv -100     voltage boost %, and an undervolt in mV under the ceiling
rochoc apply --nvvdd 1100 --msvdd 1050 --xbar 30
                                     the gated levers — passing any one arms XOC for that apply,
                                     passing none returns them to driver defaults
rochoc apply-profile "Slot 1"        apply a profile saved in the GUI
rochoc reset                         everything back to driver defaults
rochoc diag                          full dump: capabilities, raw tables, sensors
rochoc startup --enable <profile> | --disable | --status
```

`--gpu <n>` selects the card, `--mock` uses the simulated GPU. Anything that writes needs an elevated
terminal; `info`, `monitor` and `--mock` do not.

---

## What it costs while you game

The tool polls the driver only while the **hardware monitor window is open**. Close it and there is
no driver call at all — not a slower poll, none. The main window can sit on your second screen
costing nothing.

| | NVIDIA (4070 Ti) | AMD (9070 XT) |
|---|---|---|
| Monitor open | 12.97 ms / poll | 1.0–1.6 ms / poll |
| Monitor closed, fan curve running | 0.02 ms / poll | no driver call |
| Monitor closed | **no driver call** | **no driver call** |

AMD is ~10× cheaper per sample because ADL returns every sensor in one call. Tune with the monitor
open, close it before you launch the game, and the fan curve keeps running.

---

## How it works

`IGpuBackend` is the vendor-neutral seam, and `BackendFactory` picks a backend by *trying to
initialise each one* rather than sniffing device IDs.

On NVIDIA, clocks go through `SetPStates20` and limits through the client policy calls. Voltage is
the awkward one — NVIDIA exposes no voltage slider, so an undervolt is a V/F curve operation,
computed arithmetically from one absolute mV target and applied as a clock-boost lock. On AMD it is
Overdrive 8 over ADL, where a feature whose min equals its max is one the card doesn't expose, which
is how the UI knows what to hide.

Everything that can be verified is verified: writes are read back, and the status bar says when the
card reports something different from what was asked. Several vendor calls return success and then
do nothing, so "the call succeeded" is not treated as evidence.

---

## A word of warning

This writes voltage, clock and power settings to your GPU.

- Overclocking and undervolting can crash, corrupt work in progress, and in the extreme damage
  hardware. It may void your warranty.
- Change one thing at a time, test it, and write down what worked.
- **Reset** returns everything to driver defaults, and is the first thing to try if the card
  misbehaves.
- A fan curve is enforced by this app on NVIDIA (closing it hands fans back to the driver — you are
  asked first) and by the driver itself on AMD.
- On AMD, a memory timing change and a large undervolt are the likeliest causes of instability.

Provided as-is, with no warranty. You are responsible for what you do to your own hardware.

---

## Known limitations

- **The crossbar clock is tunable on Blackwell, read-only on Ada.** A 5070 Ti takes a +30 MHz offset
  and reads it back. A 4070 Ti reports a ±1000 MHz range and refuses every non-zero value while 0
  succeeds — a rejection of the value, not of the request shape, so that range is the width of the
  delta field rather than a promise.
- **Live MSVDD voltage is not readable.** Its ceiling and floor are set and read back, but the
  voltage it actually runs at is not, making it the one control here without read-back verification.
- The AMD core-clock offset is a **ceiling**, not a shift. A power-limited card will ignore it —
  check the limiter line before concluding it's broken.
- Clock snapping applies to **offsets**. A card reporting an absolute memory clock (AMD) is left
  unsnapped, since rounding its stock clock would overclock it just from reading its state.
- RDNA 4 exposes no editable V/F curve and no temperature limit; both are hidden rather than faked.
- The V/F curve editor is NVIDIA-only. Multi-GPU is implemented but untested.
- vBIOS flashing is deliberately out of scope. No signed release binaries yet.

---

## Repository layout

```
roch-gpu-oc-beta.sln       solution
assets/                    logo.svg (banner), icon.png, RochGpuOC.ico, screenshots/
build.ps1                  build + test + publish to dist\
setup.ps1                  as above, plus SDK install and launch (driven by SETUP.bat)
src/GpuTuner.Core          engine: backend abstraction, NVIDIA + AMD backends, mock, profiles, fan curve
src/GpuTuner.App           WPF GUI (RochGpuOC.exe)
src/GpuTuner.Cli           rochoc.exe, same engine headless
tests/GpuTuner.Core.Tests  dependency-free test runner (215 checks, no hardware needed)
tools/amd                  read-only PowerShell probes used to map the AMD driver surface
.github/workflows/ci.yml   build + test on Linux, publish + smoke-test on Windows
third_party/NvAPIWrapper   vendored NvAPIWrapper (LGPL-3.0) — see THIRD-PARTY-NOTICES.md
```

The loose `.bat` files are development conveniences: `REBUILD-AND-RUN.bat` wipes `bin`/`obj`/`dist`
and republishes when an incremental build goes wrong, `BUILD-GUI.bat` builds the GUI alone, and
`DIAG.bat` self-elevates and dumps `rochoc-diag.txt`. Use `build.ps1` for an ordinary build.

---

## Credits

- [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper) by Soroush Falahati — the NVAPI binding,
  vendored and retargeted to .NET 8.
- [RadeonTuner](https://github.com/dumbie/RadeonTuner) by dumbie — its Overdrive 8 code revealed the
  in/out parameter convention that had blocked the AMD backend.
- AMD's [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and
  [ADLX](https://gpuopen.com/manuals/adlx/) documentation.

## Licence

MIT — see [LICENSE](LICENSE). Third-party components keep their own licences; see
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
