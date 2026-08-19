<img src="assets/logo.svg" alt="Roch GPU OC" width="560">

# Roch GPU OC — beta

[![CI](https://github.com/RochStudio/roch-gpu-oc-beta/actions/workflows/ci.yml/badge.svg)](https://github.com/RochStudio/roch-gpu-oc-beta/actions/workflows/ci.yml)

An Afterburner-style GPU tuning tool for Windows that drives **both NVIDIA and AMD** cards from one
binary. Core clock, memory clock, voltage, power limit, fan control, live monitoring, and five
Afterburner-style profile slots.

No kernel driver. Everything goes through the vendors' own user-mode libraries — `nvapi64.dll` on
NVIDIA, `atiadlxx.dll` on AMD — the same route Afterburner, NVIDIA Inspector and AMD's own Adrenalin
software take.

> **Beta.** Tested on an RTX 4070 Ti (driver 591.86) and an RX 9070 XT (Adrenalin 25.x–26.x). Other cards
> should work — the tool asks the driver what it supports rather than assuming — but they are
> untested. Read the [warning](#a-word-of-warning) before using it.

---

## What you get

The UI is built from what the driver says the card supports, so the controls change with the card
rather than showing everything greyed out.

| Control | NVIDIA | AMD (RDNA+) |
|---|---|---|
| Core clock | offset, MHz, in 15 MHz steps | offset, MHz |
| Core voltage (%) | over-voltage, raises the ceiling | — |
| Voltage cap (mV) | absolute lock, holds the core at or below it | offset, mV (undervolt) |
| Memory clock | offset, MHz, in 25 MHz steps | absolute clock, MHz |
| Power limit | % of TDP | % offset |
| Temperature limit | ✓ | driver-owned, hidden |
| Fan | fixed %, or a software curve | fixed %, or a 5-point hardware curve |
| Zero RPM | — | ✓ |
| Memory timing | — | ✓ (fast timing) |
| V/F curve editor | ✓ | no editable curve on RDNA 4 |

Voltage is two independent levers on NVIDIA, as in Afterburner: **Core Voltage (%)** raises how far
the core may be driven above the top of the V/F table, and **Voltage Cap (mV)** holds it at or below
a chosen voltage. A card can carry both — headroom for boost, a cap to hold it under load. Set the
cap at or above the ceiling and it caps nothing.

The clock offsets snap to the driver's own granularity, so the number on the slider is the number
that reaches the card rather than one that gets quietly rounded on the way. That also means the top
of the core range is +990 rather than +1000, which is the nearest step below the driver's limit.

Plus, on both: a hardware monitor in its own window (core/memory clock, voltage, power, edge and
hot-spot and memory temperatures, load, fan % and RPM, with a session peak for core clock), five
profile slots, apply-at-logon via Task Scheduler, tray operation, and a CLI.

### The V/F curve editor

Reads the card's real table — 103 points spanning 450–1090 mV on a 4070 Ti — and plots what the card
will actually do rather than what is stored:

- **Drag a point**, or click one and use **↑/↓** to nudge it by one 5 MHz driver step (Shift for 25,
  Ctrl also flattens everything above it, ←/→ walk the selection along the curve).
- **Past an active voltage cap the points lie flat**, because the card cannot reach those voltages —
  they are drawn where the card will run, and are not editable while the cap holds them.
- **Past the last point the curve continues dashed** out to the boost ceiling. The table is fixed in
  hardware and a boost adds no points to it, so that stretch is extrapolation, not data.
- Ctrl+drag flattens from a point rightwards; double-click returns one point to stock; right-click
  returns all of them.

---

## Building from source

There is no prebuilt release yet — building it yourself is the way to run it. On a machine with the
.NET 8 SDK already present this takes well under a minute.

### 1. Prerequisites

| Requirement | Notes |
|---|---|
| **Windows 10/11 x64** | To *run* the tool. It can be *built* on Linux/macOS — see [Building off Windows](#building-off-windows). |
| **.NET 8 SDK** | `winget install Microsoft.DotNet.SDK.8` — verify with `dotnet --list-sdks` (any `8.*`). |
| **Vendor GPU driver** | The one you already have. NVIDIA ships `nvapi64.dll` and AMD ships `atiadlxx.dll` with the driver; neither is bundled here. |
| **Administrator rights** | To *run* only, not to build. Writing clocks requires elevation, and both executables request it via `app.manifest`. |

The SDK includes the runtime, so a machine that builds the tool can also run it. Running a build
made elsewhere needs only the [.NET 8 **Desktop** Runtime](https://dotnet.microsoft.com/download/dotnet/8.0)
(the Desktop variant, not the plain one — the GUI is WPF).

### 2. Clone and build

```powershell
git clone https://github.com/RochStudio/roch-gpu-oc-beta.git
cd roch-gpu-oc-beta
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

`build.ps1` compiles the solution, runs the test suite, and publishes the GUI and the CLI side by
side into `dist\`. It stops on the first failure.

If you don't have the SDK yet, `SETUP.bat` does the whole thing in one double-click: it self-elevates,
installs the .NET 8 SDK via winget if it's missing, builds, tests, publishes, writes what it detected
to `gputuner-info.txt`, and launches the GUI.

Then, from an **elevated** terminal:

```powershell
.\dist\RochGpuOC.exe
```

The `-ExecutionPolicy Bypass` is needed because the script is unsigned; it applies to that one
invocation only and does not change your machine's policy.

### 3. What lands in `dist\`

| File | What it is |
|---|---|
| `RochGpuOC.exe` | The WPF GUI. Requests elevation on launch. |
| `rochoc.exe` | The CLI — same engine, no window. See [Command line](#command-line). |
| `GpuTuner.Core.dll` | The engine both front-ends share. |
| `NvAPIWrapper.dll` | Vendored NVAPI binding. |

Both executables are **framework-dependent** `win-x64` builds (`--self-contained false`), so the
target machine needs the .NET 8 Desktop Runtime. `dist\` is gitignored.

### Building by hand

If you would rather not run the script, this is exactly what it does:

```powershell
dotnet build roch-gpu-oc-beta.sln -c Release
dotnet run --project tests/GpuTuner.Core.Tests -c Release --no-build
dotnet publish src/GpuTuner.Cli/GpuTuner.Cli.csproj -c Release -r win-x64 --self-contained false -o dist
dotnet publish src/GpuTuner.App/GpuTuner.App.csproj -c Release -r win-x64 --self-contained false -o dist
```

**Publish the CLI before the GUI.** Both publish into the same folder, and on case-insensitive NTFS
the second publish will otherwise clean up the first one's output. This ordering is not cosmetic —
reversing it produces a `dist\` missing one of the two executables.

### Building in an IDE

Open `roch-gpu-oc-beta.sln` in Visual Studio 2022 (17.8+, with the **.NET desktop development**
workload) or JetBrains Rider. Set `GpuTuner.App` as the startup project. To exercise the tuning paths
you must start the IDE itself as administrator, otherwise every write fails at the driver.

The solution contains five projects:

| Project | Target | Output |
|---|---|---|
| `src/GpuTuner.Core` | `net8.0` | `GpuTuner.Core.dll` |
| `src/GpuTuner.App` | `net8.0-windows` (WPF) | `RochGpuOC.exe` |
| `src/GpuTuner.Cli` | `net8.0` | `rochoc.exe` |
| `tests/GpuTuner.Core.Tests` | `net8.0` | test runner |
| `third_party/NvAPIWrapper` | `net8.0` | `NvAPIWrapper.dll` |

### Running the tests

```powershell
dotnet run --project tests/GpuTuner.Core.Tests -c Release
```

Expected output is `164 passed, 0 failed`, exit code 0. The runner is dependency-free — no xunit, no
NuGet restore beyond the SDK — so it builds offline and runs anywhere. It covers the V/F curve
maths, the voltage levers, clock-step snapping, profile clamping, the fan-curve controller, the
background-poll paths and apply ordering against a mock backend, so most of the engine can be
changed without a GPU in front of you.

CI runs exactly this on every push. The Linux job builds the solution and runs the suite — neither
touches a driver — and the Windows job runs `build.ps1`, checks both executables landed, and
smoke-tests `rochoc info --mock` on a runner with no GPU at all.

### Developing without a GPU

Add `--mock` to any CLI command to run against a simulated RTX 4070 SUPER. No driver call is made and
nothing touches real hardware:

```powershell
.\dist\rochoc.exe info --mock
.\dist\rochoc.exe monitor --mock
```

This is the fastest way to check that a build is sound on a machine with no supported card, and it
needs no elevation.

### Building off Windows

`GpuTuner.App` sets `EnableWindowsTargeting`, so `dotnet build` succeeds on Linux and macOS CI
agents — the WPF reference pack comes from NuGet. The result cannot run there; it is a compile check
only. `dotnet publish -r win-x64` also works cross-platform.

### If the build fails

| Symptom | Cause |
|---|---|
| `The current .NET SDK does not support targeting .NET 8.0` | SDK older than 8.0. Check `dotnet --list-sdks`. |
| `NETSDK1100: … requires the Windows Desktop` | Building `GpuTuner.App` off Windows without `EnableWindowsTargeting`. Build the `.sln`, not the bare `.csproj`. |
| `RochGpuOC.exe` missing from `dist\` | The two publishes ran in the wrong order — see [Building by hand](#building-by-hand). |
| `...\dist\... is denied` | The GUI is still running. Close it (including the tray icon) and rebuild. |
| Builds fine, every write silently does nothing | Not elevated. Run the terminal, or the IDE, as administrator. |

`.\dist\rochoc.exe diag` is the first thing to run when the *tool* builds but misbehaves — it prints
exactly what the driver reported, which is usually the answer.

---

## Command line

`rochoc.exe` is the same engine without the window — useful for scripting and for diagnosing a card.

```
rochoc info                          what was detected, and every limit the driver reports
rochoc monitor [--interval 1000]     live telemetry in the terminal, until Ctrl+C
rochoc apply --core 120 --mem 800 --power 110 --temp 85 --fan 60
rochoc apply-profile "Slot 1"        apply a profile saved in the GUI
rochoc list-profiles                 names of the saved profiles
rochoc reset                         everything back to driver defaults
rochoc diag                          full dump: capabilities, raw tables, sensors
rochoc startup --enable <profile> | --disable | --status
```

Global options: `--gpu <n>` selects the card (default 0), `--mock` uses the simulated GPU, and
`--help` prints the same list. `diag` also writes `rochoc-diag.txt` beside the working directory.

Anything that writes to the card — `apply`, `apply-profile`, `reset`, `startup` — needs an elevated
terminal. `info`, `monitor`, `list-profiles` and anything with `--mock` do not.

---

## What it costs while you game

Polling the driver every second means competing with your game for that same driver, so the tool only
does it while the **hardware monitor window is open**. Close the monitor and it stops — not a slower
poll, no driver call at all. The rule is deliberately tied to the monitor rather than to the tray,
because the monitor is the only thing that displays telemetry; the main window can sit open on your
second screen costing nothing.

Measured on an RTX 4070 Ti, driver 591.86, 1000 ms interval:

| State | CPU per 30 s | Per poll |
|---|---|---|
| Hardware monitor open | 328 ms | 12.97 ms, 198 KB |
| Monitor closed, fan curve running | — | 0.02 ms, 3.7 KB |
| Monitor closed, no fan curve | **0 ms** | no driver call |

The 0 ms is three consecutive 30-second samples of a settled process, not a rounding of something
small. Memory sits still with it:

| | Working set | Private |
|---|---|---|
| Idle, monitor closed | 118 MB | 60 MB |
| After a monitor session | ~158 MB | ~85 MB, drifting back down |
| `rochoc` CLI, same engine | 38 MB | 14 MB |

The engine is the 14 MB; the rest is WPF, and it does not grow — a three-minute soak with the monitor
open and the graphs live ended 4 MB *lower* than it started, with handle count flat. The rise after a
monitor session is the GC holding on to segments it has not returned to the OS yet, not a leak.

A full sample costs what it does because of one call: NVAPI's power-topology query is 8.8 ms of it on
its own, and `PerformanceControl.CurrentActiveLimit` accounts for 117 KB of the 198 KB. A fan curve
needs neither — only the temperature — so with the monitor closed it reads the thermal sensor and
nothing else, about 580× cheaper. That figure is **NVIDIA**: AMD returns every sensor from a
single call, so its equivalent path saves the parsing but not the call.

That background sample is stepped into the curve and dropped: never stored, never graphed, because
only its temperature field is valid. The graphs therefore show a gap for the time the monitor was
closed rather than a run of zeroes, and the limiter line under the GPU name says it is not sampling
rather than reporting a stale reading.

Measured on an RX 9070 XT, driver 26.3.1, 1000 ms interval:

| State | CPU per 30 s | Per poll |
|---|---|---|
| Full telemetry sample | 31–47 ms | 1.0–1.6 ms |
| Monitor closed | **0 ms** | no driver call |

AMD is roughly **10× cheaper per sample** than NVIDIA, because ADL's PMLog read hands back every
sensor in one call and there is no equivalent of the power-topology query to pay for. The
monitor-closed row is zero for a stronger reason than on NVIDIA: the AMD fan curve lives in the
driver, so no software curve is ever active and the background poll returns before it calls anything
at all — there is no AMD equivalent of the temperature-only sample described above, because nothing
needs one. Nothing this app can be asked to do will make it touch the driver with the monitor closed.

The AMD per-sample figure is the driver cost, taken from `rochoc monitor`, which runs the same
`PollForeground` path as the GUI; the monitor window adds its graph drawing on top of it. The 0 ms row
is the GUI itself — three consecutive 30-second samples with handle count and working set flat.

Memory on the same machine: 122 MB working set and 73 MB private idle with the monitor closed, and
29 MB / 7 MB for the CLI. Both sat still across the run, handle count included.

So: tune with the monitor open, close it before you launch the game, and the fan curve keeps running.
Raising **Poll interval** in settings scales the monitor-open row down proportionally.

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

---

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

---

## Repository layout

```
roch-gpu-oc-beta.sln       solution
assets/                    logo: icon.png (the Roch mark, shared with Roch Viewer), logo.svg (banner), RochGpuOC.ico
build.ps1                  build + test + publish to dist\
setup.ps1                  as above, plus SDK install and launch (driven by SETUP.bat)
src/GpuTuner.Core          engine: backend abstraction, NVIDIA + AMD backends, mock, profiles, fan curve
src/GpuTuner.App           WPF GUI (RochGpuOC.exe)
src/GpuTuner.Cli           rochoc.exe, same engine headless
tests/GpuTuner.Core.Tests  dependency-free test runner (164 checks, no hardware needed)
tools/amd                  read-only PowerShell probes used to map the AMD driver surface
.github/workflows/ci.yml   build + test on Linux, publish + smoke-test on Windows
third_party/NvAPIWrapper   vendored NvAPIWrapper (LGPL-3.0) — see THIRD-PARTY-NOTICES.md
```

The remaining `.bat` files are development conveniences: `REBUILD-AND-RUN.bat` wipes
`bin`/`obj`/`dist` and republishes when an incremental build goes wrong, `BUILD-GUI.bat` builds the
GUI alone into `gui-build.log`, and `DIAG.bat` self-elevates and dumps `rochoc-diag.txt`. Use
`build.ps1` for an ordinary build.

---

## Known limitations

- The AMD core-clock offset is a **ceiling**, not a shift. If the card is power-limited it will
  change nothing — check the limiter line under the GPU name before concluding it's broken.
- `ReadTemperatureOnly` — the cheap temperature-only read — is now implemented by both shipping
  backends, so nothing relies on the interface default that falls back to a full telemetry read.
  On AMD it is belt-and-braces rather than a saving: a hardware fan curve leaves no software curve
  for the background poll to feed, so the poll returns before calling anything at all. The saving is
  also structurally smaller there — ADL hands back every sensor from one `QueryPMLogData` call, so a
  curve that did need feeding would still make it, where NVAPI's cost is a per-sensor-group call and
  skipping the 8.8 ms power-topology query is most of the win.
- The 15/25 MHz clock snapping applies to **offsets**. A card reporting an absolute memory clock
  (AMD) is left unsnapped, because its stock clock is not on any such grid and rounding it would
  overclock the card just from reading its state. That path has had no hardware to test against.
- RDNA 4 exposes no editable V/F curve and no temperature limit; both are hidden rather than faked.
- Multi-GPU is implemented but untested — GPU 0 is used unless `--gpu` says otherwise.
- The V/F curve editor is NVIDIA-only.
- vBIOS flashing is deliberately out of scope.
- **The XBAR (crossbar) clock is not tunable, and not readable either.** Every published interface
  omits the domain, checked on a 5070 Ti (driver 610.88): NVAPI reports only Graphics and Memory
  across all 32 clock slots, PStates20 adds a read-only Video and nothing else, NVML lists
  Graphics/SM/Memory/Video, and NVIDIA's open kernel modules carry no `CLK_CLK_DOMAINS_*` control at
  all — their one open clock command, `PMUMON_CLK_DOMAINS_GET_SAMPLES` (0x20801037), samples GPC,
  DRAM, NVD and active-GPC, with no crossbar field. Tools that do move it drive the proprietary
  clock-domain control block (XBAR is domain index 1) through kernel RM calls, and raise the
  frequency indirectly by offsetting the shared MSVDD rail rather than setting a clock. Reaching that
  needs an undocumented Windows RM transport plus a control-block layout pinned to a specific driver
  build, and MSVDD is a shared rail with no vendor-sanctioned safe range — so it stays out for the
  same reason vBIOS flashing does. `\\.\NvAdminDevice` does open, so the transport is the missing
  piece rather than the permissions.
- No signed release binaries yet; build from source.

---

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
