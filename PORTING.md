# Porting notes (C# → Rust)

Source of truth: `../roch-gpu-src` (RochStudio/Roch-GPU at tag v1.1.0, commit 833c8e4). Port behaviour
faithfully; the C# comments explain hard-won driver facts — keep the important ones (condensed) as
Rust comments.

## Layout

| C# | Rust |
|---|---|
| `GpuTuner.Core/Models`, `IGpuBackend`, `MockBackend`, `ProfileStore`, `ProfileRecovery`, `TelemetryHistory`, `StartupTaskService` | `crates/roch-core` (done) |
| `GpuTuner.Core/Services/TuningService.cs` | `crates/roch-core/src/services/tuning.rs` |
| `Backends/Nvidia/*` + the parts of `third_party/NvAPIWrapper` it uses | `crates/roch-nvidia` |
| `Backends/Amd/*` | `crates/roch-amd` |
| `Backends/Intel/*` | `crates/roch-intel` |
| `BackendFactory`, `GpuTuner.Cli`, `GpuTuner.App` (WPF) | `crates/roch-gpu` (one exe: verb → CLI, no verb → egui window) |

Backend crates depend only on `roch-core`, so each compiles and tests on its own:
`cargo test -p roch-amd`. Use a private target dir per agent to avoid lock contention, e.g.
`CARGO_TARGET_DIR=target/amd`.

## Conventions

- Trait: `roch_core::GpuBackend` (`crates/roch-core/src/backend.rs`). Methods take `&mut self` and
  return `roch_core::Result<T>` (`GpuError { message }`). C# exceptions → `Err(GpuError::new(..))`.
  Use the `roch_core::bail!` macro for early returns.
- Never panic on driver data. No `unwrap()` on FFI results; no slicing that can go out of bounds.
- `Math.Round(x)` is banker's rounding → `roch_core::util::round_even*`. `Math.Round(x,
  MidpointRounding.AwayFromZero)` → `f64::round`. `Math.Clamp` → `roch_core::util::clamp_i32` /
  `clamp_f64` (never panic). C# int arithmetic wraps silently; use `wrapping_*`/`checked_*` where the
  C# relied on it.
- `double.NaN` "unavailable" semantics are kept as `f64::NAN`. Note `GpuTelemetry::default()` matches
  the C# record defaults (hot_spot_c/memory_temperature_c/voltage_mv default to 0, not NaN — set NaN
  explicitly as the C# does).
- DLLs: `roch_core::native::load_system32("nvapi64.dll")` (System32 only), then
  `unsafe { roch_core::native::symbol::<FnType>(&lib, "Name") }`. Keep the `Library` alive as long as
  any function pointer from it is used.
- Struct layouts for FFI: `#[repr(C)]` (or `#[repr(C, packed(N))]` where C# used `Pack = N`). Add
  `const _: () = assert!(size_of::<T>() == N);` for every struct whose C# size is known/derivable,
  and unit tests for offsets the C# tests pin down. Getting a size wrong sends garbage to a GPU
  driver — be meticulous; where a buffer-based approach (`Vec<u8>` + offsets) is what the C# does,
  do the same.
- Windows-only code behind `#[cfg(windows)]` where it touches Win32; the crates must still `cargo
  check` cleanly on Windows (that is the only target that matters).
- Tests: port the relevant checks from `tests/GpuTuner.Core.Tests/Program.cs` (and the Telemetry/App
  tests where applicable) into `#[cfg(test)]` unit tests next to the code.
- Do **not** run anything that writes to the GPU. Read-only calls against the real RTX 5070 Ti in
  this machine are fine for verification (`cargo run`/tests that only read).
- Style: rustfmt defaults with `max_width = 120` (see `rustfmt.toml`); doc comments on public items.
