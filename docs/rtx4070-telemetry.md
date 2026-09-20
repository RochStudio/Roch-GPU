# Gigabyte RTX 4070 telemetry additions

The GV-N4070GAMING OC-12GD HWiNFO screenshot was used as a comparison list.
These readings come directly from NVIDIA NVAPI/NVML and do not require
HWiNFO to run or its shared-memory feature to be enabled.

Added and observed on RTX 4070 / driver 591.86:

- Video-engine and bus-interface load.
- Available VRAM and allocated VRAM as a percentage of available capacity.
- Driver-reported video decoding clock.
- Current GPU thermal-limit target.
- Power, thermal, voltage, and no-load performance-limit flags.
- Current PCIe generation, link speed, and lane width.

The 13 added readings participate in the existing current/minimum/maximum/average
table. Limits display Yes/No, with mixed-sample averages expressed as percent
active. Unsupported sources are omitted; a missing later sample clears the
current value while preserving history. The instantaneous GPU counter is now
labelled measured rather than effective. XBAR/SYS measurement visibility does
not require writable tuning controls.

Live verification returned 1185 MHz video, 2.5 GT/s PCIe x16, 11343 MB available
VRAM, 5.56% VRAM usage, and a no-load limiter. These are samples, not constants.
The Release build and 425 core / 28 telemetry checks passed.

Not included: HWiNFO per-memory-chip temperatures, individual rail voltages and
powers unavailable through the current backend, Windows D3D usage/memory,
PCIe error counters, normalized power, and interval-averaged effective clocks.
Those require additional validated sources; no values or labels are inferred
from undocumented sensor positions. HWiNFO clock units and polling intervals
can also differ, so side-by-side numbers are not expected to match exactly.

Use `RochGPU.exe sensors` to print the supplemental readings for diagnostics.
