# Rust port audit against Roch GPU v1.1.0 (C#)

Goal: everything the Rust build shows a user matches what the C# v1.1.0 build showed, on a real
NVIDIA GeForce RTX 5070 Ti (driver 616.92, vBIOS 98.03.58.00.43, GIGABYTE board) and on the
simulated GPU. Audit run on 2026-10-17 against `roch-gpu-src` at tag `v1.1.0`.

## How

Two dump harnesses print the same normalized `key = value` lines, and a script diffs them key by
key. Live readings are prefixed `live.` and compared with a tolerance (and by eye); everything
else must match exactly.

| Piece | Where |
|---|---|
| C# harness (console app referencing `GpuTuner.App` + `GpuTuner.Core`, outside both repos) | `C:\Users\arrow\gpuoc\audit-harness\cs\` (`AuditDump.csproj`, `Program.cs`) |
| Rust harness (`#[cfg(test)]`, `#[ignore]`d — nothing ships) | `crates/roch-gpu/src/audit_dump.rs` (`audit_dump`, `audit_fmt`), plus `cfg(test)` accessors in `gui/windows/{fan,monitor,curve}.rs` |
| Settings copy (fresh temp copy of `%AppData%\Roch GPU`: settings, profiles, recovery journal) | `audit-harness\prep.ps1` |
| Run everything + diff, and check the real `settings.json` hash is unchanged | `audit-harness\run.ps1`, `audit-harness\diff.py` |

Why a test module rather than an example: `roch-gpu` is a binary-only crate, so an example cannot
reach `gui::view_model`, `gui::telemetry_table` or the windows. A cfg(test) module can, without a
hidden verb in the shipped exe.

Run: build both (`dotnet build -c Release audit-harness\cs`; in this worktree
`$env:CARGO_TARGET_DIR='target/audit'; cargo test -p roch-gpu --release --no-run`), then
`audit-harness\run.ps1` (`-Modes mock` to skip the card).

### Read-only on hardware

Every call each harness makes in `real` mode was traced before running it:

* C#: `BackendFactory.CreateAndInitialize` (NVAPI init + enumeration), `TuningService.Initialize`
  (`GetCapabilities`, `ReadTuningState`, `ReadVoltageLockMv`), `ReadTuningState`,
  `ReadVoltageLockMv`, `ReadGraphicsInfo`, `ReadVfCurve`, `MeasureExtraClocks`,
  `Backend.ReadTelemetry`, `ReadCurrentAsProfile`, the `MainViewModel` constructor (the same reads,
  the temp profile store, `schtasks /Query`), and the curve window's ceiling read. `App.Settings`
  and `App.Recovery` are pointed at the temp copy by reflection; the recovery journal's apply
  delegate is a tripwire that throws; the VM's `Telemetry` setter (which saves through
  `App.Store`, the real folder) is never used — the sample goes into its backing field.
  `svc.Dispose` unloads NVAPI/NVML only (`ReleaseFanControl` is not called).
* Rust: the same list through `TuningService`/`MainViewModel::new`. No job is queued (asserted),
  the logon task is a stub whose `register`/`unregister` panic, and the journal's apply panics.
  All NVIDIA writes live in `backend/set.rs`, the write half of `backend/curve.rs` and
  `set_clock_domain_offset` (`BACKEND_NOTES.md`), none reachable from those reads.
* Apply, levers, slot clicks and Reset run only on the mock. The C# GUI and the non-mock Rust GUI
  were not run; the real "Roch GPU Apply Profile" task was only queried. `run.ps1` checks the
  real `settings.json` hash before/after (unchanged).

### What is compared (≈2,700 keys: 1,384 mock, 1,045 real card, 304 format probes)

* **Core**: `GpuDevice` list; every `GpuCapabilities` field (86, incl. `CanSetVoltage`,
  `TempLimitUnit`); every `GpuTuningState` field; `ReadVoltageLockMv`; `VoltageLockMechanism`;
  every `GpuGraphicsInfo` field and all 17 Graphics-window rows (name, value, banding); every V/F
  point (127 on the 5070 Ti); `MeasureExtraClocks` keys; telemetry: which fields are NaN, array
  lengths, the full supplemental list (key, name, unit; values as live).
* **Service**: `StockCeilingMv`, `BoostCeilingMv`, `ReachableCeilingMv` at 0/5/25/50/75/100 %,
  `ArmedOnCard`, `SupportedLevers`, observed/seeded ceilings, rail and OCP defaults — before and
  after the view model seeds them from the (copied) settings — and every `ReadCurrentAsProfile`
  field.
* **Main window VM**: header lines (name, VRAM, driver, vBIOS, memory type/vendor, PCIe, ReBAR,
  limiter line with and without a sample), every label and range text, every editor value and
  value-box text, every `…Text` caption, `VoltageCapText`, `TargetVoltageRangeText`,
  `XocStatusText`, gate labels, slot buttons (occupied/active/summary/tooltip), profiles, startup
  tick/profile, status, `AppliedSummary`, `BuildProfileFromEditor` (what Apply would write).
* **Sliders**: min/max/step of every main-window and XOC slider as `MainWindow.xaml` /
  `XocWindow.xaml` bind them, and every value-box tooltip.
* **Fan window** opening state: note, per-fan rows/values/captions, link, curve parameters
  visibility, hysteresis/min step text, point rows, point hint, all three mode notes, Zero RPM
  visibility, and the captions for a fixed sample.
* **Curve window**: editor configuration, flatten-box bounds, cap, ceiling, the editor's axes
  (C# `VfCurveEditor.Bounds` by reflection), the "N curve points loaded…" line, edit status, and a
  flatten-from-1000 mV preview point by point.
* **Monitor**: the telemetry table (group, name, current/min/max/avg, banding) after two fixed
  samples chosen to hit rounding ties, NaN sensors, late sensors, every unit group and the
  `state` unit; the three summary cards; the sensor count; and the row layout for the real
  card's first sample and extra clocks.
* **Mock actions**: status, summary and XOC text after edit → Apply → NVVDD arm/edit/disarm →
  voltage cap + Apply → Save into slot 3 → empty slot 4 → Reset → slot 3 apply.
* **Capability variants** (mock): Intel-style (`Percent`) and AMD-style (`Offset`) capabilities
  swapped into the view model, as `tests/GpuTuner.App.Tests` does — labels, ranges, steps, value
  boxes, fan window and nudged values.
* **Number formats**: 38 values × `0`, `0.0`, `0.#`, `0.##`, `F0`–`F3` printed by .NET 10 vs the
  Rust helpers.
* **CLI**: `info`, `sensors`, `domains`, `diag` from both executables (after the fixes).

## Results

| Category | Result |
|---|---|
| Devices, capabilities, tuning state, voltage lock/mechanism | match |
| Graphics window (17 rows) | match (Driver Date: harness artefact, see below) |
| V/F curve, extra-clock keys, telemetry fields, supplemental sensor list | match (values live) |
| Service ceilings, armed/supported levers, `ReadCurrentAsProfile` | match |
| Main-window header, labels, values, value boxes, captions, tooltips, slots, status, summary | match |
| Slider ranges and steps (main + XOC), XOC texts and gate labels | match |
| Fan window opening state | match (point hint **fixed** after the merge, see below) |
| Curve window texts, axes, flatten preview | match |
| Telemetry table, fixed samples (values, rounding, groups, banding) | match |
| Telemetry table, real card layout | **fixed** (domain rows were out of order) |
| Monitor summary cards, fan captions, tray tooltip, VRAM/memory size | **fixed** (tie rounding) |
| Mock actions (Apply, levers, slots, Reset) | match |
| Intel/AMD capability variants | match |
| .NET number-format probes (304) | match (after the fix) |
| CLI `info` / `sensors` / `domains` / `diag` | match apart from live readings (`diag` line endings: known deviation) |

## Fixes

1. **Clock-domain rows out of order in the Monitor** — `bf06cb9`.
   `measure_extra_clocks` returns a `BTreeMap`, so the keys reached `TelemetryTable::new` in text
   order (`domain20, domain22, domain3, domain31, domain6`): the real card showed L2CLK and the
   reference clock above HUBCLK and DISPCLK. C#: `NvApiBackend.MeasureExtraClocks` inserts them in
   ascending type from `ExtraDomainTypes` (a `SortedSet<int>`, `NvApiBackend.cs` ~807–867) and
   `TelemetryTable` iterates that order (`TelemetryTable.cs:135–138`). The table now sorts the
   domain rows by type. Test: `telemetry_table::tests::clock_domains_are_listed_by_type_not_by_key_text`.
2. **Tie rounding where v1.1.0 used .NET custom formats** — `332d4f2`.
   .NET custom formats (`0`, `0.0`, `0.#`, `0.##`) take the value to 15 significant digits and
   round half away from zero; Rust's `{:.N}` rounds the exact binary value half to even. Visible
   on the real card's kind of figures: 45.25 °C → C# "45.3", Rust "45.2"; 2422.5 MHz → "2423" vs
   "2422"; 1010.5 rpm → "1011" vs "1010". New `roch_core::util::dotnet_custom` (pinned against
   .NET 10 output) is used at every site that was a custom format in C#:
   Monitor cards (`MonitorWindow.xaml.cs` `Summary`, "0.0"/"0"), fan captions
   (`FanWindow.xaml.cs:316–318`), fan point boxes (`:162–163`, `:245–246`), fan-curve drag label
   (`FanCurveEditor.cs:178`), tray tooltip (`MainWindow.xaml.cs:83`), VRAM text
   (`MainViewModel.cs:146,151`, "0.#"), Graphics memory size (`GpuGraphicsInfo.cs:39`, "0.##" via
   `trim_decimals`, which also serves the AMD diagnostics `0.###` and the OCP error text `0.##`).
   The telemetry table keeps `{:.N}`: its C# `F<n>` formats round half to even as well, which the
   probes confirmed — so `GUI_NOTES.md` no longer lists "`F<n>` rounding" as a deviation.
   Tests: `util::tests::custom_formats_match_dotnet`, `format::tests::one_decimal_trims`,
   `monitor::tests`, `fan::tests::texts`, `graphics_info` tests.

Harness commits: `d85d4b4` (dump + accessors; the fan window's fixed-speed note, row label and
point hint moved into helpers so the dump reads the code the window draws), `2775a82` (variants).

3. **Fan point hint wording** — after merging with the performance branch. The hint now reads
   exactly as `FanWindow.xaml.cs:165` ("N points  ·  Enter to apply") on every card; the mock,
   Intel and AMD variants all match when the harness is re-run on the merged branch.

## Known deviations (not fixed)

* **Offset slider ranges (deliberate, requested after the audit)** — v1.1.0 narrowed the core,
  memory, XBAR, SYS and video offsets to "practical" windows (core −150…+750, memory −250…+4000,
  the private domains −150…+750). The Rust build offers the driver's full reported ranges — on the
  RTX 5070 Ti core −1000…+1000, memory −1000…+3000, XBAR/SYS/video −1000…+1000 MHz — and the
  driver's own limits are reachable even off the 15/25 MHz grid. `info` therefore prints those
  wider ranges, and the slider ranges in the window differ from v1.1.0 accordingly.

* **Fan curve point limit on hardware curves (Intel/AMD)** — behaviour only: the editor refuses
  to add points past `fan_curve_points` where v1.1.0 let the driver reject them. The hint text
  itself now matches v1.1.0 ("N points  ·  Enter to apply"); it previously read "N of M points"
  and showed "6 of 5 points" for a saved curve longer than the card's table.
* **Simulated GPU telemetry** — the Rust mock uses its own xorshift PRNG where the C# used
  `new Random(42)`, so the mock's random-walk readings differ (all mock structure matches).
* **Number culture** — C# formatted telemetry numbers, fan boxes and the VRAM text with the current
  culture; the Rust build always uses `.`. Identical on en-US (this machine); a comma-decimal
  locale would have shown "45,2 °C" in v1.1.0.
* **Driver Date when not elevated** — C# `WindowsDisplayIdentity.DriverDate` aborts the whole
  lookup when it meets the access-restricted `Properties` subkey of the display class, so an
  unelevated C# process shows "—"; the Rust version skips that subkey and shows the date. The C#
  GUI always runs elevated, where it shows the same date as Rust (2026-09-04 here), so the GUIs
  match; the difference in the dump is a harness artefact (the C# harness ran unelevated).
* `MainViewModel.DeviceLine` (C# legacy combined line) is bound nowhere in v1.1.0's XAML and was
  not ported. `fixed_fan_input` exists only in the Rust API (no window shows it).
* Already listed in `BACKEND_NOTES.md`/`GUI_NOTES.md` and unchanged: `diag` uses `\n` line
  endings; `--mock` skips elevation; child windows close on hide-to-tray; egui modals instead of
  MessageBoxes; Monitor/Curve driver reads off the UI thread.

## Not covered

* AMD and Intel hardware (none in this machine): only the capability variants on the mock.
* The startup-task toggle and anything else that registers the logon task or writes the card on
  real hardware: compared by reading both sources (`MainViewModel.UpdateStartupTask` vs
  `view_model.rs` `queue_startup_update`/`finish`), not run.
* Pixel layout, colours and fonts.
