# AMD developer tools

The historical PowerShell probes are replaced by native Rust commands. Run from the repository root:

| Command | Purpose |
|---|---|
| `cargo xtask amd probe` | Installed driver libraries, ADL adapter identity, Overdrive capabilities and live PMLog sensors |
| `cargo xtask amd exports` | Actual PE export names, legacy OD8 getter variants and every AMD adapter index |
| `cargo xtask amd features` | OD8 capability bits, feature names, min/max/default/current values and X3 adjustability |
| `cargo xtask amd telemetry` | Legacy power/temperature/performance APIs and a three-second streaming PMLog sample |
| `cargo xtask amd write-check --apply` | Four core-clock write shapes, readback and live clocks; restore and verify the original OD8 snapshot after each attempt |

Common options: `--adapter INDEX`, `--out report.txt`, and `--mock`. Mock mode never loads a driver and exercises every command on simulated data. Write-check accepts `--offset MHz` (default 200), validates the driver's range, and requires `--apply` on real hardware. It restores the original values, including nonzero offsets, rather than resetting them to zero. An early error also attempts restoration; a failed restoration is reported.

Examples:

```console
cargo xtask amd probe --out amd-report.txt
cargo xtask amd features --adapter 0
cargo xtask amd write-check --mock
cargo xtask amd telemetry --mock
```

Reports go to the terminal unless `--out` is supplied. The tools load driver DLLs from System32 only, release ADL output buffers, and stop/destroy streaming devices on exit and failure. They do not use PowerShell, .NET, HWiNFO or another helper executable.

Tests cover malformed PE data, rejected options, read-only commands, write/readback/restore with simulated failures, and streaming cleanup. Real AMD hardware validation is separate from these tests.
