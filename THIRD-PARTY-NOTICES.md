# Third-party notices

Roch GPU is GPL-3.0-or-later (see `LICENSE`).

The Rust version contains no vendored third-party source. Rust crate dependencies are listed in
`Cargo.lock` and carry their own licences (MIT / Apache-2.0 for the most part); run
`cargo about` or `cargo license` to produce the full list for a release.

## NVIDIA NVAPI / NVML

No NVIDIA code or headers are included. `crates/roch-nvidia` calls `nvapi64.dll` and `nvml.dll`,
both shipped with the NVIDIA driver and loaded from System32 only.

The NVAPI structure layouts, function identifiers and the cooler/usage aggregation logic in
`crates/roch-nvidia/src/nvapi.rs` and `wrapper.rs` are a Rust translation of the parts of
**NvAPIWrapper** (https://github.com/falahati/NvAPIWrapper, Soroush Falahati, LGPL-3.0) that the
C# version of this tool vendored and used. LGPL-3.0 §2(b) permits conveying the library under the
plain GPL-3.0, so the translated portions are distributed here under GPL-3.0-or-later as part of
the whole; the complete corresponding source is this repository.

## AMD Display Library (ADL) / ADLX

No AMD code or headers are included. `crates/roch-amd` is an independently written binding to
`atiadlxx.dll` and the ADLX runtime, which ship with the AMD driver. The signatures, enumeration
values and structure layouts are AMD's published interface
(https://gpuopen-librariesandsdks.github.io/adl/).

## RadeonTuner

- **Upstream:** https://github.com/dumbie/RadeonTuner

No code from RadeonTuner is included. It was read as a reference for how the Overdrive 8 API
expects to be called — specifically that `lpNumberOfFeatures` is an in/out parameter.

## Intel Graphics Control Library (IGCL)

`crates/roch-intel` is an independently written binding to the public
[IGCL API](https://github.com/intel/drivers.gpu.control-library). It loads `ControlLib.dll` from
Windows System32. No Intel headers, driver binaries, or Arc Power code are redistributed.
