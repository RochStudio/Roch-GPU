# RDNA4 clocks and timing support

Investigated on 2026-09-08 using the local RX 9070 XT and its read-only ADL diagnostics.

## Implemented

- PMLog FCLK (sensor 44) and SoC clock (sensor 3), in MHz. Unsupported readings remain NaN and are omitted from the telemetry table.
- Memory timing choices generated from the driver's inclusive range. UI/profile indices are translated to driver IDs at the backend boundary. Undocumented IDs use neutral names. The tested card reports only 0 and 1.
- Core control explicitly labelled as an offset; the UI explains the absolute-limit and FCLK limitations.

## Outstanding: clock writes

The tested card reports GfxClkFMax -500..1000 with default 0 (offset semantics) and GfxClkFMin 0..0 (locked). These do not provide an absolute minimum/maximum pair. Reusing the existing NVIDIA clock-lock controls would misrepresent support.

ADL's public PMLog FCLK entry establishes telemetry, not a setter. No verified Windows RDNA4 FCLK setter was found. Do not substitute SoC clock for fabric clock or send guessed SMU messages.

Adrenalift documents RDNA4 soft core limits through an InpOut helper driver, but is closed source. An independently implemented low-level backend requires a verified RDNA4 transport, message semantics and readback before enabling controls. That work is not implemented here.

## Sources

- AMD ADL sensor definitions: https://github.com/GPUOpen-LibrariesAndSDKs/display-library/blob/master/include/adl_defines.h
- AMD OD8 sample and capability requirements: https://gpuopen-librariesandsdks.github.io/adl/Overdrive8-example.html
- Adrenalift feature reference (closed source): https://github.com/miklebel/adrenalift
- Linux RDNA4 soft core limit research: https://gitlab.com/fpsflow/power_limit_removal

No hardware tuning writes were performed for this change.
