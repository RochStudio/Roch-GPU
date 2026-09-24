# Intel Arc Pro B60 support

Roch GPU loads the installed Windows x64 `ControlLib.dll` from System32 and uses Intel's public [Graphics Control Library](https://github.com/intel/drivers.gpu.control-library) [API](https://intel.github.io/drivers.gpu.control-library/Control/api.html). It does not need Intel Graphics Software to remain open. Discovery and monitoring do not change tuning. IGCL's tuning waiver is enabled only for writes.

## Tested hardware

Intel Arc Pro B60, 24 GB, PCI device E211, driver 32.0.101.9030. Validated September 23, 2026.

| Control | Driver-reported range | Roch GPU units |
|---|---|---|
| Core offset | −300 to +1000 MHz | MHz, 1 MHz steps |
| Memory speed | 19–22 Gbps | 19000–22000 Mbps, 1 Mbps steps |
| Voltage limit | 0–100% | Percentage of voltage headroom |
| Power limit | 50–120% | Percentage |
| Temperature limit | 60–100% | Percentage of thermal margin |
| V/F curve | 10 points, 400–1500 mV / 0–4300 MHz | Frequency editing, 10 MHz steps |
| Fan control | Automatic, fixed duty, up to 10 hardware curve points | °C / percent duty |

These are API bounds, not recommended overclocks. Capabilities are discovered per device; other Arc models and driver versions have not been validated. NVIDIA XBAR/SYS/rail controls and AMD timing controls are not exposed on Intel.

## Driver behavior handled

- Scalar setters reject out-of-range/off-grid values and verify readback. Unchanged scalar settings are skipped, preserving existing custom curves during a general Apply.
- V/F writes become visible asynchronously. Readback waits up to 500 ms and checks returned frequencies. Voltage coordinates can move with operating conditions, so editing uses a freshly read grid. The graph reloads actual returned points.
- Resetting a custom curve uses a zero core offset; replaying a previously read stock voltage grid can cause Intel to interpolate different frequencies. This resets the core offset to zero too.
- Fan curves must have distinct increasing temperatures and non-decreasing duties. A falling duty can make this B60 driver report success while reverting to automatic mode. Such curves are rejected before writing. Readback verifies mode, count, units and every point.
- This B60 reports inconsistent fan mode/unit flags. Its reported ten-point capacity and existing percent table are checked, with a device-specific compatibility path. Fixed duty is implemented as a flat hardware curve and reads back as Fixed in Roch GPU.
- Fan curves are run by the driver. There is no separate Intel zero-RPM switch in this build; actual zero-RPM behavior remains firmware-dependent.
- Manual V/F edits are applied from the curve editor; the existing five profile slots store scalar tuning and fan curves, not per-point V/F edits. A changed core offset can replace a custom V/F curve.

## Earlier implementation validation

All 461 core, 34 telemetry, and 15 app checks pass. Intel profile loading and applying reject profiles saved for a different GPU, preventing old NVIDIA clock offsets from being interpreted as Intel memory speeds.

Live write/readback/restore checks passed for core −1 MHz, voltage/power/temperature −1 percentage point, memory +1 Mbps, a middle V/F point −50 MHz, and a middle fan point +1 percentage point. Fixed 40% duty, automatic fans, restoring the original ten-point curve, and applying the current full profile also passed.

Restored state: core offset 0 MHz, memory 19 Gbps, voltage limit 100%, power 120%, temperature 100%, stock V/F frequencies, and the original fan curve: (25,30), (34,30), (41,30), (49,30), (57,30), (66,30), (74,48), (81,70), (89,80), (97,90), expressed as °C / %.

Telemetry includes reported GPU/memory temperatures, core/memory clocks, voltage, energy-derived board power, utilization, VRAM allocation, fan RPM, VR temperatures and memory bandwidth when supported. Power/load need two samples; unsupported readings remain unavailable instead of showing fabricated zeroes. Functional tests do not establish gaming or stress-test stability.

The B60 telemetry header identifies Battlemage BMG-G21 WKSTN. Additional native sensors include global/core and SA VR temperatures, memory voltage, separate GPU/memory power, render/media activity, available VRAM and usage, PCIe speed/width, and throttle flags. Memory bandwidth has its own group. These additions were verified by read-only sampling on the B60.

VBIOS, PCIe interface and Resizable BAR now populate below the driver line. See [additional Arc feature research](intel-arc-research.md) for the B60 memory-voltage findings and candidate features.

## Core clock range

The Intel main window now exposes minimum/maximum core frequency limits when its GPU domain reports control support and permits readback. Enable the range checkbox, edit both MHz limits, then Apply. Uncheck and Apply to request the driver's factory range. Profiles save these limits independently of core offset and fan settings. The existing range is read on startup, so opening the app does not apply a new limit.

On this B60 the advertised range is 400–3000 MHz, but the factory range reads 400–2400 MHz. Hardware maxima and factory defaults are distinct: reset uses Intel's -1/-1 factory sentinel, not 400/3000. Applied values are read back; failed or mismatched writes attempt to restore and verify the previous range. Frequency limits are requested bounds, not a guarantee of achieved clocks under thermal/power constraints.

Validation: 466 core, 34 telemetry and 18 app checks pass (518 total). A live 400–2300 MHz write/readback test passed, factory reset returned 400–2400 MHz, and exact previous tuning state, fan configuration and V/F curve were verified after restoration. No increased upper clock, voltage or power limits were tested. See [Arc Power security review](arc-power-security-review.md).

## Version 1.0.9 review and validation

The latest checks are recorded in [1.0.9 notes](releases/1.0.9.md). The editor uses 15 MHz core, 100 Mbps memory, and 5% voltage/power/temperature increments; the table above describes the finer native API granularity. Loading existing off-grid settings does not round them.

Stock profiles use the driver's reported voltage-limit default (0% on this B60/driver). UI driver reads and voltage-cap writes are serialized with telemetry. Software fan updates cannot overwrite a newly selected fan mode. These shared-service fixes also apply to the other backends; they were regression-tested without changing the physical card's tuning.
