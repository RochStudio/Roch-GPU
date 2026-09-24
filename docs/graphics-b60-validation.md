# Graphics tab validation — Intel Arc Pro B60

The Graphics window now includes Intel board vendor, native Xe core count, PCI revision, memory type and bus width. B60 device ID E211 maps to Battlemage BMG-G21 WKSTN and Intel's documented TSMC N5 process. VBIOS and PCI bus address are included for every backend. The rows scroll at smaller window sizes. Driver dates prefer the matching driver version and provider, avoiding the integrated Intel GPU's date.

Validation on 2026-09-23:
- Core: 476 passed, 0 failed.
- App: 18 passed, 0 failed.
- Telemetry: 34 passed, 0 failed.
- Release publish succeeded: `dist/b60-graphics-fix/RochGPU.exe`.
- Read-only production backend readback: ASRock, 20 Xe cores, revision 00, 24 GB GDDR6, 192 bit, PCIe 5.0 x8, VBIOS 23.1066.00.00, PCI address 0000:03:00.0, driver 32.0.101.9030 dated 2026-09-17. The driver reports Resizable BAR disabled.
- Isolated WPF Graphics window launched successfully with live values. Computer-use screenshot approval timed out, so visual layout and interactive scrolling were not verified.
- GPU tuning was not changed. The existing tuner remained open to preserve unapplied edits.
- Memory vendor and ROP/TMU counts remain unavailable; no guessed values are displayed.

Intel B60 specifications: https://www.intel.com/content/www/us/en/products/sku/243916/intel-arc-pro-b60-graphics/specifications.html

The 1.0.9 app regression suite now checks the production Graphics XAML/resources in WPF: all 17 rows bind, board identity displays, vertical overflow scrolls through the last row, and light/dark resources change. These automated layout checks do not replace manual visual inspection.
