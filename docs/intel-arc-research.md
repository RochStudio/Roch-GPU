# Intel Arc tuning research — 2026-09-23

Scope: Windows Arc Pro B60 (8086:E211), driver 32.0.101.9030. All probes for this investigation were read-only; no tuning values or firmware were changed.

## Memory voltage

Not adjustable through this card's current public IGCL interface. `ctlOverclockGetProperties` reports the VRAM voltage capability unsupported, with all-zero limits. `ctlOverclockVramVoltageOffsetGet` returns `0x4000000A` (unsupported feature). The approximately 1.352 V sensor is a measurement, not an editable target. Intel marks the old VRAM voltage/frequency-offset properties as deprecated. This does not establish that every possible board-level or firmware method is impossible; it establishes that Roch GPU has no verified supported control on this configuration.

## Additional features

| Feature | Finding for this B60 | Roch GPU status |
| --- | --- | --- |
| Custom V/F curve | Public IGCL curve API works in earlier write/readback tests | Already implemented; saving individual curve points in profiles remains a useful extension |
| Core frequency min/max | Frequency-domain properties report canControl=true, 400–3000 MHz | Candidate for an additional clock-range control; no writes tested in this investigation |
| Memory/media frequency range | Both domains report canControl=false | Do not expose editable range controls |
| Legacy voltage/frequency lock | Getter returns 0x44000009 (deprecated API) | Not available through this legacy route |
| Negative legacy core-voltage offset | Getter returns deprecated API | Alchemist repository implementations are not evidence of B60 support |
| Sustained/burst power limits | Public IGCL/Level Zero APIs and other projects expose separate power domains | Further capability/unit/interaction checks needed; existing percentage limit remains the verified control |
| FPS limiter, low latency, frame synchronization, per-game profiles | Present in Arc Power through graphics/driver integrations | Potential separate graphics settings, not extra electrical overclock controls |
| Extended Alchemist limits | Arc Power documents legacy-runtime and Sysman methods for A770-class hardware | Not verified or transferable as B60 limits |

## Identity implemented

The header shows VBIOS, maximum PCIe interface and Resizable BAR immediately below the driver line. IGCL initialization requests its firmware discovery support, with fallback for older runtimes. Only the `OptionRomCode` component is treated as VBIOS; GSC firmware and Option ROM data are different components. Failed queries remain unavailable.

Live readback:

- Option ROM code: 23.1066.00.00.
- GSC firmware: BMG__21.1182 (not substituted for VBIOS).
- PCI address: 0000:03:00.0.
- Maximum interface: PCIe 5.0 x8. Current telemetry at measurement: 16 GT/s x8 (Gen 4).
- Resizable BAR: supported, disabled according to IGCL.

Validation: 461 core, 34 telemetry and 15 application tests pass (510 total), including PCI structure offsets, enabled/disabled/unsupported ReBAR states, and selecting ROM code rather than ROM data. Native readback confirmed the real adapter's identity and continued telemetry.

## Sources reviewed

- [Intel IGCL API/header](https://github.com/intel/drivers.gpu.control-library/blob/master/include/igcl_api.h): public capabilities, deprecated VRAM controls, clock domains and PCI information.
- [Intel overclocking sample](https://github.com/intel/drivers.gpu.control-library/blob/master/Samples/Overclocking_Sample/Sample_OverclockAPP.cpp): property discovery and overclock APIs.
- [Arc Power feature details](https://github.com/YamsSE/Arc-Power/blob/main/docs/features.md): capability-gated expert controls, Alchemist undervolting and extended limits. These are project claims and not B60 verification.
- [Arc Power repository](https://github.com/YamsSE/Arc-Power): graphics controls and profiles. Its broad Arc Pro support description differs from this B60's observed working OC support; local driver capability checks take precedence.
- [Arc Pro fan-control Windows implementation](https://github.com/exzile/intel-arc-pro-fan-control/blob/master/windows/src/arc.cpp) and [README](https://github.com/exzile/intel-arc-pro-fan-control/blob/master/windows/README.md): B60/B70 fan tables, OC, power and profile persistence. Reviewed as a comparison; no implementation copied.
- [Intel IGSC introduction](https://github.com/intel/igsc/blob/master/doc/introduction.rst): distinction between GSC firmware and Option ROM code/data identities.
