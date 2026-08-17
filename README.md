# Roch GPU OC — beta

An Afterburner-style GPU tuning tool for Windows that drives **both NVIDIA and AMD** cards from one
binary. Core clock, memory clock, voltage, power limit, fan control, live monitoring, and five
Afterburner-style profile slots.

No kernel driver. Everything goes through the vendors' own user-mode libraries — `nvapi64.dll` on
NVIDIA, `atiadlxx.dll` on AMD — the same route Afterburner, NVIDIA Inspector and AMD's own Adrenalin
software take.

> **Beta.** Tested on an RTX 4070 Ti (driver 591.86) and an RX 9070 XT (Adrenalin 25.x). Other cards
> should work — the tool asks the driver what it supports rather than assuming — but they are
> untested. Read the [warning](#a-word-of-warning) before using it.

---

## What you get

The UI is built from what the driver says the card supports, so the controls change with the card
rather than showing everything greyed out.

| Control | NVIDIA | AMD (RDNA+) |
|---|---|---|
| Core clock | offset, MHz | offset, MHz |
| Core voltage | absolute target, mV (V/F curve cap or boost) | offset, mV (undervolt) |
| Memory clock | offset, MHz | absolute clock, MHz |
| Power limit | % of TDP | % offset |
| Temperature limit | ✓ | driver-owned, hidden |
| Fan | fixed %, or a software curve | fixed %, or a 5-point hardware curve |
| Zero RPM | — | ✓ |
| Memory timing | — | ✓ (fast timing) |
| V/F curve editor | ✓ | no editable curve on RDNA 4 |

Plus, on both: a hardware monitor in its own window (core/memory clock, voltage, power, edge and
hot-spot and memory temperatures, load, fan % and RPM, with a session peak for core clock), five
profile slots, apply-at-logon via Task Scheduler, tray operation, and a CLI.

## Install

Needs **Windows 10/11 x64**, the **.NET 8 Desktop Runtime**, and the vendor driver you already have.
Run as administrator — writing clocks requires it, and the app asks via its manifest.

```powershell
git clone https://github.com/<you>/roch-gpu-oc-beta.git
cd roch-gpu-oc-beta
powershell -ExecutionPolicy Bypass -File .\build.ps1
.\dist\RochGpuOC.exe
```

Building needs the [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
(`winget install Microsoft.DotNet.SDK.8`).

## Command line

`rochoc.exe` is the same engine without the window — useful for scripting and for diagnosing a card.

```
rochoc info                          what was detected, and every limit the driver reports
rochoc monitor                       live telemetry in the terminal
rochoc apply --core 120 --mem 800 --power 110 --temp 85 --fan 60
rochoc apply-profile "Slot 1"
rochoc reset                         everything back to driver defaults
rochoc diag > diag.txt               full dump: capabilities, raw tables, sensors
```

`diag` is the first thing to run when something doesn't behave — it prints exactly what the driver
reported, which is usually the answer.

## How it works

`IGpuBackend` is the vendor-neutral seam. `BackendFactory` picks a backend by *trying to initialise
each one* rather than sniffing device IDs — a library that loads, initialises and enumerates a GPU is
by definition the one that works on that machine.

**NVIDIA.** Clock offsets go through `SetPStates20`; the power and thermal limits through the client
policy calls. Voltage is the awkward one: NVIDIA exposes no voltage slider, so an undervolt is a V/F
curve operation. The tool computes the flatten arithmetically from one absolute mV target
(`VfCurve.cs`, unit-tested), applies it as a clock-boost lock, verifies by read-back, and falls back
to an NVML locked-clock cap when a driver accepts the private lock and then ignores it.

**AMD.** Overdrive 8 over ADL. The driver hands back a capability bitmask plus a per-feature table of
min/max/default; a feature whose min equals its max is one this card doesn't expose, which is how the
UI knows to hide the V/F curve and temperature limit on RDNA 4. Two details cost real time to find
and are worth knowing if you're reading the code: `lpNumberOfFeatures` is an **in/out** parameter that
must arrive pre-set (passing 0 returns `ADL_ERR_NULL_POINTER`, which reads like "unsupported" but
isn't), and the OD8 tables are handled as plain integer buffers rather than declared structs so no
packing assumption can corrupt a write.

Everything that can be verified is verified: writes are read back, and the status bar says when the
card reports something different from what was asked. Several vendor calls return success and then do
nothing, so "the call succeeded" is not treated as evidence.

## A word of warning

This writes voltage, clock and power settings to your GPU.

- Overclocking and undervolting can crash, corrupt work in progress, and in the extreme damage
  hardware. It may void your warranty.
- Change one thing at a time, test it, and write down what worked.
- **Reset** returns everything to driver defaults. It is also the first thing to try if the card
  misbehaves after a change.
- Fan control: a curve set here is enforced by this app on NVIDIA (so closing it hands fans back to
  the driver — the app asks before letting that happen) and by the driver itself on AMD (so it
  survives closing the app).
- On AMD, a memory timing change and a large undervolt are the two most likely causes of instability.

Provided as-is, with no warranty. You are responsible for what you do to your own hardware.

## Repository layout

```
roch-gpu-oc-beta.sln     solution
src/GpuTuner.Core        engine: backend abstraction, NVIDIA + AMD backends, mock, profiles, fan curve
src/GpuTuner.App         WPF GUI (RochGpuOC.exe)
src/GpuTuner.Cli         rochoc.exe, same engine headless
tests/GpuTuner.Core.Tests  dependency-free test runner (112 checks, no hardware needed)
tools/amd                read-only PowerShell probes used to map the AMD driver surface
third_party/NvAPIWrapper vendored NvAPIWrapper (LGPL-3.0) — see THIRD-PARTY-NOTICES.md
```

Run the tests with `dotnet run --project tests/GpuTuner.Core.Tests -c Release`. They cover the curve
maths, the voltage planner, profile clamping, the fan-curve controller and the apply ordering against
a mock backend, so most of the engine can be changed without a GPU in front of you.

## Known limitations

- The AMD core-clock offset is a **ceiling**, not a shift. If the card is power-limited it will
  change nothing — check the limiter line under the GPU name before concluding it's broken.
- RDNA 4 exposes no editable V/F curve and no temperature limit; both are hidden rather than faked.
- Multi-GPU is implemented but untested — GPU 0 is used unless `--gpu` says otherwise.
- The V/F curve editor is NVIDIA-only.
- vBIOS flashing is deliberately out of scope.

## Credits

- [NvAPIWrapper](https://github.com/falahati/NvAPIWrapper) by Soroush Falahati — the NVAPI binding,
  vendored and retargeted to .NET 8.
- [RadeonTuner](https://github.com/dumbie/RadeonTuner) by dumbie — reading its Overdrive 8 code is
  what revealed the in/out parameter convention that had blocked the AMD backend.
- AMD's [ADL](https://gpuopen-librariesandsdks.github.io/adl/) and
  [ADLX](https://gpuopen.com/manuals/adlx/) documentation.

## Licence

MIT — see [LICENSE](LICENSE). Third-party components keep their own licences; see
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
