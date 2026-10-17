# roch-nvidia API map (for porting `NvApiBackend.cs`)

One line per public item, with the C# member it replaces. `h` is always `nvapi::GpuHandle`
(`PhysicalGPU.Handle` / `handle.MemoryAddress` in the C#). Nothing here panics on driver data.

Conventions carried over from the C#:
- `nvapi::*` getters return `NvResult<T>`; `NvError::Api(status)` = `NVIDIAApiException`
  (`ex.Status` → `status_text()`, `ex.Message` → `message()`), `NvError::NotSupported` =
  `NVIDIANotSupportedException`. `GpuError: From<NvError>`.
- `private::*` keeps `NvApiPrivate`'s style: raw NVAPI status `i32` (0 ok, -1 not exported), `Option<String>`
  = "null on success, else the reason", empty `Vec`/map when the family is absent.
- C# default parameters became either a second function (`read_curve_raw_with`) or an explicit
  argument (`entry_flag: Option<i32>` for `entryFlag = -1`, `SWEEP_MAX_WORDS` for `maxWords = 20`).
- `buf` (scratch buffers, `rd_i32`/`wr_i32`) is `pub(crate)`; use it from the backend if needed.

## nvapi.rs — NvAPIWrapper replacement

| Rust | C# |
|---|---|
| `initialize()` / `unload()` | `NVIDIA.Initialize()` / `NVIDIA.Unload()` |
| `library_loaded()` | (DllImport load of nvapi64; System32 only) |
| `driver_version()` | `NVIDIA.DriverVersion` (e.g. 61692) |
| `error_message(status)` | `GeneralApi.GetErrorMessage` |
| `status_name(status)` | `Status.ToString()` |
| `enum_physical_gpus()` | `PhysicalGPU.GetPhysicalGPUs()` (handles only) |
| `query_interface(id)` / `exposes(id)` / `unsafe resolve::<F>(id)` | `nvapi_QueryInterface` / DelegateFactory |
| `unsafe call_struct(id, h, &mut T)`, `get_versioned::<T>(id, h, ver)`, `new_versioned::<T>(ver)`, `zeroed::<T>()`, `version_word(size, ver)` | generic struct call helpers (`StructureVersion`) |
| `RawFn`, `Status`, `OK`, `INCOMPATIBLE_STRUCT_VERSION`, `NOT_SUPPORTED`, `id::*` | delegate shape, `Status`, `FunctionId` |
| `full_name` / `short_name` | `PhysicalGPU.FullName` / `ArchitectInformation.ShortName` |
| `bus_id` / `bus_slot_id` / `bus_type` | `BusInformation.BusId` / `.BusSlot` / `.BusType` |
| `pci_identifiers` / `pci_identifiers_checked` → `PciIdentifiers` | `GPUApi.GetPCIIdentifiers` / `BusInformation.PCIIdentifiers` |
| `vbios_revision` / `vbios_oem_revision` / `vbios_version_string` / `bios_version_string` | `GPUApi.GetVBIOS*` / `Bios.VersionString` |
| `gpu_core_count` / `rop_count` | `ArchitectInformation.NumberOfCores` / `.NumberOfROPs` |
| `ram_type` / `ram_maker` / `ram_bus_width` | `GPUApi.GetRAMType` / `GetRAMMaker` / `GetRAMBusWidth` |
| `tach_reading` | `GPUApi.GetTachReading` |
| `current_pstate` | `GPUApi.GetCurrentPerformanceState` |
| `pstates20(h)` → `PStates20 { is_editable, clocks }`, `.clocks_for(id)` | `GPUApi.GetPerformanceStates20` (V3→V2→V1) |
| `set_pstates20_v1(h, &mut PStates20InfoV1)` | `GPUApi.SetPerformanceStates20` |
| `memory_info(h)` → `MemoryInfo` | `PhysicalGPU.MemoryInformation` (V3→V2→V1) |
| `clock_frequencies(h, type)` (0 current, 1 base, 2 boost) | `Current/Base/BoostClockFrequencies` |
| `thermal_settings(h)` → `Vec<ThermalSensor>` | `ThermalInformation.ThermalSensors` (V2→V1, target All) |
| `power_policies_info` / `power_policies_status` / `set_power_policies_status` | `GPUApi.ClientPowerPoliciesGetInfo` / `GetStatus` / `SetStatus` (`PerformanceControl.PowerLimit*`) |
| `power_topology_status` | `PowerTopologyInformation.PowerTopologyEntries` |
| `thermal_policies_info` / `thermal_policies_status` / `set_thermal_policies_status` | `GPUApi.GetThermalPoliciesInfo` / `GetThermalPoliciesStatus` / `SetThermalPoliciesStatus` (`PerformanceControl.ThermalLimit*`) |
| `performance_limit(h)` | `PerformanceControl.CurrentActiveLimit` |
| `current_voltage_uv(h)` | `GPUApi.GetCurrentVoltage().ValueInMicroVolt` |
| `core_voltage_boost_percent` / `set_core_voltage_boost_percent` | `GPUApi.Get/SetCoreVoltageBoostPercent` |
| `clock_boost_mask(h)` | `GPUApi.GetClockBoostMask` (`_Masks`) |
| `clock_boost_table_gpu_deltas(h)` | `GPUApi.GetClockBoostTable` (zero mask; prefer `private::read_deltas*`) |
| `vfp_curve_entries(h)` | `GPUApi.GetVFPCurve` (zero mask; prefer `private::read_curve*`) |
| `clock_boost_ranges(h)` | `GPUApi.GetClockBoostRanges` |
| `clock_boost_locks` / `set_clock_boost_locks` | `GPUApi.GetClockBoostLock` / `SetClockBoostLock` |
| `enable_overclocked_pstates(h)` | `GPUApi.EnableOverclockedPStates` |
| `dynamic_pstates(h)` / `usages(h)` | `UsageInformation` (DynamicPstatesInfoEx / GetUsages) |
| `cooler_settings(h)` / `set_cooler_level` / `restore_cooler_settings` | legacy `GPUApi.GetCoolerSettings` / `SetCoolerLevels` / `RestoreCoolerSettings` |
| `fan_coolers_info` / `fan_coolers_status` / `fan_coolers_control` / `set_fan_coolers_control` | `GPUApi.ClientFanCoolersGetInfo` / `GetStatus` / `GetControl` / `SetControl` |
| structs (`ClockFrequencies`, `PStates20InfoV1/V2`, `ClockBoostLockV2`, `VfpCurveV1`, `ClockBoostTableV1`, `FanCoolers*V1`, ...) | `Native/GPU/Structures/*` (size-asserted) |
| `pstate_name`, `clock_domain_name`, `clock_domain_defined`, `lock_mode_name`, `thermal_controller_name`, `thermal_target_name`, `cooler_policy_name`, `performance_limit_name`, `memory_type_name`, `flags_name` | enum `.ToString()` / `Enum.IsDefined` |

The wrapper's aggregation objects are rebuilt in `wrapper.rs`: `coolers` (`CoolerInformation.Coolers`:
legacy cooler family, else client fan coolers), `set_cooler_settings` / `restore_coolers_to_default`
(`SetCoolerSettings` / `RestoreCoolerSettingsToDefault`), `utilization_domains` (`UsageInformation`),
`pcm_percent` / `thermal_info_c` (the `PerformanceControl` views' unit conversions). The backend itself
is `backend/` — see `BACKEND_NOTES.md`.

## nvml.rs — `Nvml` (process-global session, nvml.dll from System32)

| Rust | C# |
|---|---|
| `is_available()` / `last_error()` / `session_epoch()` | `Nvml.IsAvailable` / `LastError` / `SessionEpoch` |
| `lock_graphics_clocks(i, min, max)` / `reset_graphics_clocks(i)` → `Option<String>` | `LockGraphicsClocks` / `ResetGraphicsClocks` |
| `power_watts(i)` (NaN when unknown) | `PowerWatts` |
| `max_graphics_clock_mhz(i)` / `min_graphics_clock_mhz(i)` | `MaxGraphicsClockMhz` / `MinGraphicsClockMhz` |
| `resizable_bar_enabled(i)` | `ResizableBarEnabled` |
| `pcie_link(i)` → `PcieLink` | `PcieLink` (tuple) |
| `device_name(i)` | `DeviceName` |
| `session_may_be_stale(code)` | `SessionMayBeStale` |
| `try_shutdown()` | `TryShutdown` |

## private.rs — `NvApiPrivate`

Thermal sensors (0x65FE3AAD)
| Rust | C# |
|---|---|
| `probe_mask(h)` | `ProbeMask` |
| `read_thermal_slots(h, mask)` (+ pure `parse_thermal_slots`) | `Read` |
| `interpret(name, &slots)` → `(hotspot, memory)` | `Interpret` |
| `probe_thermal(h)` → `Vec<ThermalProbe>` | `ProbeThermal` |
| `read_thermal_channels(h)` → `Vec<ThermalChannel>` (+ `parse_thermal_channels`) | `ReadThermalChannels` |

V/F curve and delta table
| Rust | C# |
|---|---|
| `FULL_MASK`, `MASK_103`, `mask_for(&deltas)` | `FullMask`, `Mask103`, `MaskFor` |
| `curve_available()` | `CurveAvailable` |
| `read_curve(h, &mask)` → `Vec<(uv, khz)>` (103 struct points) | `ReadCurve` |
| `read_deltas(h, &mask)` / `write_deltas(h, &mask, &deltas)` | `ReadDeltas` / `WriteDeltas` |
| `read_curve_raw(h, &mask, entries, ver)` / `read_curve_raw_with(.., mem_entries, tail_uints)` → `(status, Vec<(mv, mhz)>)` | `ReadCurveRaw` |
| `read_deltas_raw(h, &mask, entries, ver)` → `(status, Vec<i32>)` | `ReadDeltasRaw` |
| `write_deltas_raw(h, &mask, entries, ver, &deltas, entry_flag)` | `WriteDeltasRaw` (`entryFlag=-1` → `None`) |
| `read_trailing_arrays(h, &mask, entries, ver)` | `ReadTrailingArrays` |
| `probe_curve_layouts(h)` → `Vec<CurveProbe>` | `ProbeCurveLayouts` |
| `struct_sizes()` | `StructSizes` |
| `total_points(g)`, `MASK_POINTS`, `struct_points`, `delta_points`, `delta_offset`, `curve_table_size`, `delta_table_size`, `HEADER_BYTES`, ... | `TotalPoints`, `MaskPoints`, private layout helpers |
| `trailing_array()` / `set_trailing_array(v)` | `TrailingArray` get/set |
| pure: `parse_curve_raw`, `parse_curve_struct`, `parse_deltas_raw`, `parse_deltas_struct`, `stage_deltas_raw`, `parse_trailing_arrays` | buffer bodies of the above |

Voltage rails
| Rust | C# |
|---|---|
| `RAIL_NVVDD` / `RAIL_MSVDD` | `RailNvvdd` / `RailMsvdd` |
| `read_rail_mask(h)` | `ReadRailMask` |
| `read_volt_rails(h)` → `Vec<VoltRail>` (+ `parse_volt_rails`) | `ReadVoltRails` |
| `write_volt_rail_min_offset(h, rail, uv)` / `write_volt_rail_max_offset(h, rail, uv)` | `WriteVoltRailMinOffset` / `WriteVoltRailMaxOffset` |
| `dump_rail_status(h)` (+ `parse_rail_status_dump`) | `DumpRailStatus` |
| `read_rail_voltages_uv(h)` → `BTreeMap<rail, uv>` (+ `parse_rail_voltages`) | `ReadRailVoltagesUv` (ADC) |
| `rail_slot`, `rail_control_field`, `CONTROL_*` | private layout constants |

OCP / power
| Rust | C# |
|---|---|
| `OCP_SLOT_NVVDD` / `OCP_SLOT_MSVDD` | `OcpSlotNvvdd` / `OcpSlotMsvdd` |
| `write_ocp_limit(h, slot, ma)` → `Option<String>` (legacy guard < R615, modern layout >= R615) | `WriteOcpLimit` |
| `read_ocp_channels(h)` → `Vec<OcpChannel>` (+ `parse_ocp_channels`) | `ReadOcpChannels` |
| `read_ocp_ranges(h)` → `Vec<OcpRange>` (+ `parse_ocp_ranges`) | `ReadOcpRanges` |
| `range_for(&ranges, limit_ma)` | `RangeFor` |
| `read_power_rails(h)` → `Vec<PowerRail>` (+ `parse_power_rails`, `POWER_SHAPES`) | `ReadPowerRails` (tuples) |

Clock domains (crossbar / SYS / video)
| Rust | C# |
|---|---|
| `DOMAIN_CORE/XBAR/MEMORY`, `SLOT_CORE/XBAR/SYS/VIDEO` | `DomainCore`..., `SlotCore`... |
| `measure_clock_khz(h, domain)` | `MeasureClockKhz` |
| `read_xbar_info(h)` → `XbarInfo` (+ `parse_xbar_info`) | `ReadXbarInfo` |
| `read_domain_entries(h)` → `Vec<DomainEntry>` (+ `parse_domain_entries`) | `ReadDomainEntries` |
| `read_domain_offset_mhz(h, slot)` / `read_xbar_offset_mhz(h)` | `ReadDomainOffsetMhz` / `ReadXbarOffsetMhz` |
| `write_domain_offset_mhz(h, slot, mhz)` / `write_xbar_offset_mhz(h, mhz)` | `WriteDomainOffsetMhz` / `WriteXbarOffsetMhz` |
| `control_offset_for(slot)` | `ControlOffsetFor` |
| `dump_domain_info(h, slot)` (+ `format_domain_info`) / `dump_domain_block(h, slot)` | `DumpDomainInfo` / `DumpDomainBlock` |
| `explore_xbar_control(h)` → `Vec<ControlExplore>` | `ExploreXbarControl` |
| `probe_xbar_control_shapes(h)` | `ProbeXbarControlShapes` |
| `probe_xbar(h)` → `XbarProbe` (+ `parse_xbar_probe`; `type_one_index: Option` for `-1`) | `ProbeXbar` |

Generic / diagnostics
| Rust | C# |
|---|---|
| `call_raw(h, id, size, ver, mask_offset: Option, mask)` → `(status, words)` | `CallRaw(..., maskOffset=-1 → None, out status)` |
| `sweep_shapes(h, id, min, max, &versions, max_words)` → `Vec<ShapeHit>` (8 KB slack per try) | `SweepShapes` |
| `exposes(id)` | `Exposes` |
| `CALL_SLACK` | `CallSlack` |

## vf_curve.rs — `VfCurve` (pure)

| Rust | C# |
|---|---|
| `VfPoint { voltage_uv, frequency_khz }` | `VfPoint` |
| `compute_flatten_deltas(&pts, cap_uv, extra_khz, min, max)` (pass `i32::MIN/MAX` for the C# defaults) | `ComputeFlattenDeltas` |
| `compute_target_deltas(&pts, &BTreeMap<i32,i32>, min, max)` | `ComputeTargetDeltas` |
| `infer_cap_voltage_uv(&pts)` / `max_voltage_uv(&pts)` | `InferCapVoltageUv` / `MaxVoltageUv` |

## ocp.rs — `OcpWriteGuard` + `ModernOcpControl` (pure; closures do the driver calls)

| Rust | C# |
|---|---|
| `legacy_apply(driver, current_ma, requested_ma, write)` → `Option<String>` | `OcpWriteGuard.Apply` |
| `modern_apply(slot, ma, get, info, set)` → `Option<String>`, `CONTROL_SIZE`, `CONTROL_VERSION`, `RANGE_SIZE` | `ModernOcpControl.Apply`, `ControlSize`... |

## domain_offset.rs — `DomainOffsetControl` (pure)

| Rust | C# |
|---|---|
| `apply(slot, mhz, get, set)` → status | `DomainOffsetControl.Apply` |
| `offset_field(&control, slot)` → byte offset or -1 (layout 10: +0x10C, 15: +0x114) | `OffsetField` |
| `SIZE`, `VERSION`, `INVALID_REQUEST`, `INVALID_LAYOUT`, `VERIFICATION_FAILED` | same constants |
