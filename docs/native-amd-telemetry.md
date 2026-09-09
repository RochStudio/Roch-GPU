# Native AMD telemetry

Roch GPU reads the installed AMD driver through ADL PMLog and ADLX. It does not
read HWiNFO shared memory, require HWiNFO, or load a third-party monitoring driver.
The telemetry path is read-only and does not apply tuning settings.

## Verified on this RX 9070 XT

PowerColor RX 9070 XT, AMD driver 26.3.1, vBIOS 023.008.000.068:

| Reading | Source | Example from a live idle sample |
|---|---|---|
| GPU / hotspot / memory junction | PMLog (ADLX fallback) | 30 / 30 / 30 °C |
| Core voltage | PMLog (ADLX fallback) | 749 mV |
| Core / memory clocks | PMLog (ADLX fallback) | 269 / 910 MHz |
| Board power | PMLog (ADLX fallback) | 34 W |
| Fan speed / duty | PMLog (ADLX fallback) | 1016 RPM / 31% |
| GPU / memory-controller utilization | PMLog | Supported |
| Dedicated VRAM used — newly working | ADLX | 292 MB |
| Shared GPU memory used — new | ADLX revision 2 | 67 MB |
| PCIe generation / lanes / maximum generation — new | PMLog | 4 / 16 / 5 |

These are different acquisition times, not calibration comparisons to another
application. Existing temperature, voltage, power and clock channels are **not**
claimed as new sensors. The new memory readings replace the old unavailable/zero
allocation value; they are not copied from another monitoring application.

## Conditional support, not verified extra readings on this card

The mapper also supports native FCLK and SoC clocks, UVD/VCE/VCN clocks, core and
memory VRM temperatures, SoC temperature/VRM, GCD/MCD hotspots, liquid/intake and
bridge temperatures, SoC/memory voltage, core/SoC/ASIC watts, dGPU power limit and
throttling percentages. Rows appear only when the driver supplies valid readings.
ASIC watts are kept separate from total board watts. A supported-but-zero power
limit is hidden, not shown as a real zero-watt limit. Failed readings clear the
current value while preserving previously collected min/max/average statistics.

On this card the public PMLog interface does **not** expose FCLK, SoC clock, the
additional voltage rails or VRM temperatures. Streaming PMLog exposes the same
sensor subset. ADLX returns 12 supported metrics but no FCLK/SoC/effective clocks.
The legacy OD6 power APIs return errors or a zero placeholder; all seven ODN
temperature calls and ODN performance status return ADL_NOT_SUPPORTED (-8).
Those unsuccessful legacy probes are not added to the polling loop.

GPU effective clock and FCLK effective are **not implemented**. They must not be
estimated from GPU utilization or relabeled from ordinary clocks. Additional SMU
telemetry would require a separately verified RDNA4 access path; this change does
not install a kernel driver, disable security features or guess register offsets.

## Validation

`dotnet run --project tests/GpuTuner.Core.Tests -c Release`

`dotnet run --project tests/GpuTuner.Telemetry.Tests -c Release`

The latter compiles the production telemetry table and checks grouped insertion,
late sensor discovery, disappearing sensors, deduplication, reset and striping.
`build.ps1` runs both test suites before publishing.

`RochGPU.exe diag` reports the merged native sample and ADLX availability.
`tools/amd/NativeTelemetryProbe.ps1` is an optional read-only developer probe for
ADL adapter 0, not a runtime dependency. It starts/stops only telemetry sampling.

## API references

### Additional Windows clock-provider check

The [RadeonMon implementation](https://github.com/amu2mod/RadeonMon/blob/main/src/adlx.cpp)
uses the same public ADLX GPU clock and VRAM clock metrics; it does not provide a
separate effective core, FCLK or SoC clock reader to integrate.
[Adrenalift](https://github.com/miklebel/adrenalift) documents richer telemetry
through an InpOut low-level helper in Advanced mode, but is closed source. Its
repository does not supply an auditable RDNA4 metrics reader. No helper driver was
installed and no security settings were changed during this check.

The telemetry UI now keeps one column heading fixed above the scrolling sensors,
uses right-aligned readings with subdued historical columns, and adds three live
summary cards. This is a UI improvement, not a claim of newly working clocks.

- [AMD ADL sensor identifiers](https://github.com/GPUOpen-LibrariesAndSDKs/display-library/blob/master/include/adl_defines.h)
- [AMD ADLX metric interfaces](https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/blob/main/SDK/Include/IPerformanceMonitoring.h)
- [ADLX metrics revision 2](https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/blob/main/SDK/Include/IPerformanceMonitoring2.h)
- [ADLX metrics revision 3](https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/blob/main/SDK/Include/IPerformanceMonitoring3.h)
