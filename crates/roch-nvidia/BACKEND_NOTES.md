# NVIDIA backend port — status notes

Port of `src/GpuTuner.Core/Backends/Nvidia/NvApiBackend.cs` + `NvidiaSupplementalTelemetry.cs`
(tag v1.1.0) onto the already-ported layers (`nvapi`, `private`, `nvml`, `vf_curve`, `ocp`,
`domain_offset`).

## Status: complete

Every member of `NvApiBackend` is ported. Verified read-only against an RTX 5070 Ti (driver 616.92)
with `cargo run -p roch-nvidia --example probe`: capabilities, tuning state and the diagnostics dump
match the reference C# build (`RochGPU.exe info` / `sensors` / `diag`) field for field; the only diff
lines in `diag` are live sensor readings. Setters are ported but were never executed (no writes on
the dev machine).

## Files

| File | C# |
|---|---|
| `src/backend/mod.rs` | class, `Initialize`, `Gpu()`, `NvmlIndexFor`, `ReadGraphicsInfo` (+ `NvidiaParts`, `TechnologyFor`, `MemoryTypeName`, `MemoryMakerName`), `DomainName`, `PowerRailName`, `Dispose` (→ `Drop`), the `GpuBackend` impl, constants (`VoltageBoostHeadroomMv`, `VoltageRailHeadroomMv`, `MinLockableMhz`, `MaxDomainType`) |
| `src/backend/caps.rs` | `GetCapabilities` (+ `RailCaps`, pure `rail_caps`, `voltage_offset_min_mv`) |
| `src/backend/telemetry.rs` | `ReadTemperatureOnly`, `ReadTelemetry`, `ForgetLockAcrossDriverReset`, `ReadTuningState`, `ReadClockDomains`, `SetClockDomainOffset`, `MeasureExtraClocks`, `ExtraDomainTypes`, `OcpNow` |
| `src/backend/curve.rs` | `CurveMask`, `CurveLayout`, `ReadEffectiveCurve`, `ReadCurveDeltas`, `ReadBaseCurve`, `BoostDeltaRange`, `SetVoltageLock`, `TryWriteBoostLock`, `EnableOcPStates`, `ClearNvmlClockCap`, `CurveFrequencyAtMv`, `ReadVoltageLockMv`, `ReadBoostLockMv`, `WriteCurveDeltas`, `SetVoltageCurveOffset`, `ReadVfCurve`, `SetVfCurveTargets` |
| `src/backend/set.rs` | `SetRailEnd` (4 rail setters), `SetClockRange`, `SetDomain` (xbar/sys/video), `SetClockDelta` (core/mem), `SetPowerLimit` (4 payload strategies), `SetVoltageBoost`, `SetTempLimit`, `WriteOcp`, `SetFanSpeed`, `SetFanAuto`, `ResetToDefaults` |
| `src/backend/diagnostics.rs` | `GetDiagnostics` (+ .NET number-format helpers `fixed`, `grouped`) |
| `src/supplemental.rs` | `NvidiaSupplementalTelemetry` (`read`, `pcie_speed`) |
| `src/wrapper.rs` | NvAPIWrapper aggregation: `GPUCoolerInformation.Coolers/SetCoolerSettings/RestoreCoolerSettingsToDefault`, `GPUUsageInformation.UtilizationDomainsStatus`, `PerformanceControl` unit conversions |
| `examples/probe.rs` | read-only hardware smoke test |

## Public API added for the CLI / GUI

- `roch_nvidia::ClockDomain { slot, type_, has_range, min_mhz, max_mhz, measured_mhz, offset_mhz }` (all `i32` except `has_range: bool`)
- `NvidiaBackend::read_clock_domains(&mut self, gpu) -> Vec<ClockDomain>` (empty on a bad index)
- `NvidiaBackend::set_clock_domain_offset(&mut self, gpu, slot, mhz) -> i32` (driver status; **writes**)
- `NvidiaBackend::domain_name(type_) -> String`, `NvidiaBackend::power_rail_name(index, total) -> String` (also free fns `roch_nvidia::domain_name` / `power_rail_name`)
- `GpuBackend::as_any_mut` returns `Some(self)`.

## Deviations (deliberate)

- Curve mask / layout / write recipe / extra-domain-type caches are per GPU (the C# kept one per
  backend); identical on a single-GPU box.
- `GpuError` replaces the C# exception types; where a C# path let an `ArgumentException` or
  `NullReferenceException` escape (cooler-id mismatch across the fan-cooler lists, a missing GPU
  utilization domain, `Math.Clamp` with an inverted driver range), the Rust treats it as "no data" /
  skips the clamp instead of failing the whole read.
- The diagnostics text uses `\n` line endings (the C# wrote `\r\n` via `AppendLine`).
- NVML pairing is by adapter name (as the C#'s `NvmlIndexFor`), not PCI bus. `ReadGraphicsInfo`
  passes the NVAPI ordinal to NVML for the PCIe/ReBAR rows, also as the C# did.
- The C# test "every backend declares its own ReadTemperatureOnly" has no Rust equivalent (trait
  overrides can't be introspected); the override exists in `backend/mod.rs`.

## Safety audit (read paths)

`initialize`, `capabilities`, `read_graphics_info`, `read_telemetry`, `read_temperature_only`,
`read_tuning_state`, `read_voltage_lock_mv`, `read_vf_curve`, `measure_extra_clocks`,
`read_clock_domains`, `diagnostics` reach only getters (NVAPI Get*, private GET/measure/probe calls,
NVML queries). Every write (`set_*`/`write_*` in `private`, `nvapi::set_*`, `enable_overclocked_pstates`,
`nvml::lock/reset_graphics_clocks`, `wrapper::set_cooler_settings`/`restore_coolers_to_default`) is
called only from `backend/set.rs`, the write half of `backend/curve.rs`, and
`set_clock_domain_offset`. `EnableOverclockedPStates` is sent only from curve writes and boost-lock
recipes 2/3 — never from `capabilities`.
