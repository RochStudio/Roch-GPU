# NVIDIA driver compatibility and OCP

## Current source

Changed OCP requests on R615+ use the modern NVAPI control layout. The earlier candidate's blanket block has been replaced. Legacy OCP reads can succeed on a driver that rejects legacy writes; read success alone is not proof of write compatibility.

The modern path validates the control header, channel mask, type-19 NVVDD/MSVDD entries, positive limits and driver-reported minimum/default/maximum. It preserves the fresh getter payload except for the selected limit and verifies the selected rail and unchanged companion rail after writing. It does not retry a failed modern setter with the legacy layout. Unchanged live values are verified no-ops. Unknown driver identity blocks changed OCP writes.

The driver interfaces remain 0x8B3E7343 (control read), 0x67F31384 (bounds) and 0xAFFC2279 (write). Full allocation sizes and field layouts are documented in ModernOcpControl.cs; allocation size must not be inferred from the low 16 bits of a version word.

## Verification

Hardware: RTX 5070 Ti, NVIDIA 616.92, vBIOS 98.03.58.00.43.

- Ordinary backend test: power 90%, core offset -15 MHz and memory offset -100 MHz applied and read back; original 100% / 0 / 0 restored.
- Authorized OCP prototype test: NVVDD 300 -> 290 -> 300 A; MSVDD 120 -> 110 -> 120 A. Every setter returned success and every read-back matched. Both original limits restored.
- Packaged candidate diagnostics succeeded and detected the GPU, driver and both OCP limits.
- The owner subsequently confirmed that the updated app works, including OCP. Exact user-tested OCP values were not supplied.
- Regression checks: 362 core tests and 17 telemetry tests passed.

The scripted integrated-backend probe was prepared but not run; owner confirmation is separate from an automated end-to-end test. Interrupted-write recovery, all settings, other boards and other drivers are not exhaustively verified. Driver 616.56 was the reported failure version and has not been tested directly.

## Limits and troubleshooting

No above-default OCP write was performed in the controlled tests. Expanding an editing range is separate from applying a current limit. Large driver-reported bounds are not safe electrical recommendations. The existing UI range policy was not expanded by this compatibility fix.

Fully exit the old application, including the tray instance, before opening a replacement build. Keep any recovery journal if a write fails. Record GPU model, driver, vBIOS, executable version and `RochGPU.exe diag` output when reporting compatibility problems. No driver downgrade, firmware modification or protection bypass was used.

These notes describe the current source/local candidate, not a promise that an older published release contains the fix.
