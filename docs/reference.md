> Detailed reference retained from the previous README. Historical version notes and screenshots may describe older builds; see the [current README](../README.md) for installation and source version.

<img src="../assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU

[![CI](https://github.com/RochStudio/Roch-GPU/actions/workflows/ci.yml/badge.svg)](https://github.com/RochStudio/Roch-GPU/actions/workflows/ci.yml)

A GPU tuning tool for Windows that drives **both NVIDIA and AMD** cards from **one executable** —
clocks, voltage, power and temperature limits, per-fan control and curves, live telemetry, a V/F
curve editor, and five profile slots.

It asks the driver what your card supports instead of assuming: every range on screen is one the
driver reported, and a control for something the card hasn't got is hidden rather than greyed out.
Behind an **XOC** gate sit the NVVDD and MSVDD rail ranges, their over-current limits in amps, and
the crossbar, SYS and video clock domains — private driver families that no public NVAPI call
exposes. Each is armed on its own and off by default.

No kernel driver: everything goes through the vendors' own user-mode libraries (`nvapi64.dll`,
`atiadlxx.dll`). **`RochGPU.exe` is the whole program** — run it with nothing for the window, with a
command for the CLI. Nothing to install, no .NET runtime, no DLLs beside it.

> Writing clocks and voltages to a GPU can crash the machine, corrupt work in progress, and in the
> extreme damage hardware. Read [the warning](#a-word-of-warning) before using it.

---

## Screenshots

| Main window | XOC | Telemetry |
|---|---|---|
| <img src="../assets/screenshots/main.png" alt="Main window on an NVIDIA card" width="230"> | <img src="../assets/screenshots/xoc.png" alt="XOC window" width="250"> | <img src="../assets/screenshots/monitor.png" alt="Telemetry" width="290"> |

<img src="../assets/screenshots/fan.png" alt="Fan control" width="480">

<img src="../assets/screenshots/curve.png" alt="V/F curve editor" width="840">

The same binary on an **RX 9070 XT** comes out differently, because the window is built from what the
card reports — a voltage offset rather than a curve, an absolute memory clock, memory timing and zero
RPM, and no XOC or curve editor at all:

| Main window | Telemetry |
|---|---|
| <img src="../assets/screenshots/amd-main.png" alt="Main window on an RX 9070 XT" width="230"> | <img src="../assets/screenshots/amd-monitor.png" alt="Telemetry on an RX 9070 XT" width="330"> |

---

## Getting started

1. **Download** `RochGPU.exe` from the [latest release](https://github.com/RochStudio/Roch-GPU/releases/latest). One file, ~67 MB.
   Put it in a folder you own, not `Program Files`.
2. **Run it.** SmartScreen warns on first run because the binary is unsigned — **More info → Run
   anyway**. If you'd rather not, [build it yourself](#building-from-source).
3. **Approve the admin prompt.** Writing clocks needs it. Read-only CLI commands (`info`, `monitor`)
   don't, and work from any terminal.
4. **Check the header** names your GPU, driver and vBIOS. If not, run `.\RochGPU.exe info` — it
   prints what it found and why.
5. **Change one thing**, press **Apply**, and read the status line: it reports what actually reached
   the card, read back rather than echoed.
6. **Test it** — a benchmark or a game for a few minutes. Watch the **Telemetry** window, especially
   the limiter column, which says what's holding the card back.
7. **Keep it.** Pick a profile slot and **Save**. Tick **Startup** to apply it at logon, with the app
   left in the tray so a software fan curve keeps running.

If anything misbehaves, **Reset to Defaults** puts the card and the sliders back to the driver's own
values. That's always safe and the first thing to try.

---

## What it does

| Control | NVIDIA | AMD (RDNA+) |
|---|---|---|
| Core clock | offset, 15 MHz steps | offset, MHz |
| Memory clock | offset, 25 MHz steps | absolute clock |
| Voltage boost | %, raises the ceiling | — |
| Voltage cap | in the curve editor's flatten | offset, mV (undervolt) |
| NVVDD / MSVDD rail | floor and ceiling, mV — MSVDD is Blackwell-only | — |
| NVVDD / MSVDD OCP | over-current limit, A — Blackwell | — |
| XBAR / SYS / video clock | offset, MHz — XBAR writable on Blackwell only | — |
| Clock range | pin the graphics clock, bounds from the driver | — |
| Power limit | % of TDP | % offset |
| Temperature limit | ✓ | driver-owned, hidden |
| Fan | a duty per fan, or a software curve | one duty, or a hardware curve |
| Zero RPM / memory timing | — | ✓ |
| V/F curve editor | ✓ | no editable curve on RDNA 4 |

**Telemetry** shows every sensor the card reports with min, max and running average — including
**measured rail current**, seven 12 V channels read from the card's own power monitor rather than
divided out of the board watts. **Fan control** is its own window, a duty per fan or a curve, and
applying there saves into the active profile slot so what you set is what comes back at logon.

Nothing polls the driver unless a window is showing live readings, so the main window can sit on a
second screen costing nothing while you play.

Offsets snap to the driver's granularity, so the number on the slider is the number that reaches the
card. Core and crossbar go to **−150…+750 MHz** against the ±1000 the driver claims — that ±1000 is
the width of its delta field, not a claim about the silicon. Both ends are slider ends rather than
measurements: nothing up there is known to be stable.

**Tested on:** RTX 5070 Ti (Blackwell, full feature set), RTX 4070 Ti and 4070 (Ada — no MSVDD rail,
crossbar reads but won't take a write), RX 9070 XT (RDNA 4).

---

## Command line

Same executable, same engine, no window — the first word decides which half runs. It's a
GUI-subsystem binary, so the shell doesn't wait for it; redirect (`RochGPU.exe diag > out.txt`) to
capture output.

```
info                          what was detected, and every limit the driver reports
monitor [--interval 1000]     live telemetry until Ctrl+C
apply --core 120 --mem 800 --power 110 --fan 60
apply --volt 25 --uv -100     voltage boost %, and an undervolt in mV under the ceiling
apply --nvvdd 1100 --msvdd 1050 --xbar 30 --sys 45 --video 30
apply --nvvdd-ocp 280 --msvdd-ocp 110      OCP current limits, in whole amps
apply --clock-min 1500 --clock-max 1800    pin the graphics clock
apply-profile "Slot 1"        apply a profile saved in the GUI
reset                         everything back to driver defaults
diag                          full dump: capabilities, raw tables, sensors
startup --enable <profile> | --disable | --status
```

`--gpu <n>` selects the card, `--mock` uses a simulated GPU. Anything that writes needs an elevated
terminal. Passing a gated flag arms that lever for that apply; any lever you don't name goes back to
the driver's own value.

**Startup** registers a Scheduled Task rather than a Run key, because that's the only way to run
elevated without a prompt at every logon — so it won't appear in Task Manager's Startup tab. It
leaves the app in the tray, since a software fan curve needs something alive to step it.

---

## Building from source

Needs **Windows 10/11 x64** and the [.NET 10 SDK](https://dotnet.microsoft.com/download/dotnet/10.0)
(`winget install Microsoft.DotNet.SDK.10`).

```powershell
git clone https://github.com/RochStudio/Roch-GPU.git
cd Roch-GPU
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

Builds, tests and publishes `dist\RochGPU.exe`. `SETUP.bat` does the lot in one double-click,
including the SDK. Tests are `286 passed, 0 failed` and dependency-free — the project has no NuGet
packages at all, so most of the engine can be changed without a GPU in front of you.

If the build fails: `does not support targeting .NET 10.0` means an SDK older than 10.0;
`dist\... is denied` means the app is still running, tray icon included; and writes that silently do
nothing mean it isn't elevated.

---

## A word of warning

This writes voltage, clock and power settings to your GPU.

- Overclocking and undervolting can crash, corrupt work in progress, and in the extreme damage
  hardware. It may void your warranty.
- Change one thing at a time, test it, and write down what worked.
- The **OCP limits are protection, not performance.** Raising one doesn't make a card faster; it
  moves the current at which the card stops itself out of the way, and the reason that limit exists
  is the hardware behind it. Lower is safer than stock; higher is the opposite.
- **Reset to Defaults** writes the driver's defaults to the card and loads them into the sliders. It
  is the first thing to try if the card misbehaves.
- A fan curve is enforced by this app on NVIDIA (closing it hands the fans back, and you're asked
  first) and by the driver itself on AMD.
- On AMD, a memory timing change and a large undervolt are the likeliest causes of instability.

Provided as-is, with no warranty. You are responsible for what you do to your own hardware.

---

## Known limitations

- **A display driver reset drops the tune.** A game that hangs the card hard enough (`nvlddmkm` in
  the event log) takes the voltage lock with it, though clock and memory offsets survive. Apply
  again. The app itself recovers.
- **Nothing guards an apply-at-logon that crashed the machine.** Hold Shift during logon to skip it,
  or clear it with `startup --disable` from another account or safe mode.
- **Live MSVDD voltage isn't readable**, making it the one control here without read-back
  verification. Its ceiling and floor are set and read back normally.
- **A clock range can't be read back**, only remembered — NVML won't report one. The window forgets
  it after a driver reset rather than claiming a range nothing holds.
- The AMD core-clock offset is a **ceiling**, not a shift; a power-limited card will ignore it.
- RDNA 4 exposes no editable V/F curve and no temperature limit — both hidden rather than faked.
- The V/F curve editor is NVIDIA-only. Multi-GPU is implemented but untested.
- vBIOS flashing is deliberately out of scope, and the release binary is unsigned.

What the driver does and doesn't expose — the private families, which calls come back empty, and how
each was established — is in **[docs/driver-notes.md](driver-notes.md)**.

---

## Credits

- [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper) by Soroush Falahati — the NVAPI binding,
  vendored and retargeted to .NET 10.
- [RadeonTuner](https://github.com/dumbie/RadeonTuner) by dumbie — its Overdrive 8 code revealed the
  in/out parameter convention that had blocked the AMD backend.
- AMD's [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and
  [ADLX](https://gpuopen.com/manuals/adlx/) documentation.

## Licence

GPL-3.0-or-later. See [LICENSE](../LICENSE), the same licence as
[Roch Viewer](https://github.com/RochStudio/Roch-Viewer).

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. It writes clocks
and voltages to a graphics card; run it on hardware you are willing to experiment with.

Third-party components keep their own licences; see
[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md).
