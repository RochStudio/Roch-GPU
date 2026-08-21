# Third-party notices

Roch GPU itself is MIT licensed (see `LICENSE`). It includes and depends on the following.

---

## NvAPIWrapper — vendored, modified

- **Location in this repository:** `third_party/NvAPIWrapper/`
- **Upstream:** https://github.com/falahati/NvAPIWrapper
- **Author:** Soroush Falahati
- **Licence:** GNU Lesser General Public License v3.0 — full text at
  `third_party/NvAPIWrapper/LICENSE`

This copy has been **modified**. The changes are:

1. Retargeted to `net8.0` (upstream targets an older framework).
2. Added an `InternalsVisibleTo` attribute for `GpuTuner.Core`, so the tuning code can reach the
   internal structure definitions it needs for private NVAPI calls.

The complete, modified source of the library is included in this repository, and it is built as its
own assembly (`NvAPIWrapper.dll`) rather than merged into the application. Anyone receiving this
software can therefore study, modify, rebuild and relink it, as LGPL-3.0 §4 requires.

---

## AMD Display Library (ADL)

No AMD code or headers are included in this repository. `src/GpuTuner.Core/Backends/Amd/` contains an
independently written P/Invoke binding to `atiadlxx.dll`, which ships with the AMD graphics driver.
The function signatures, enumeration values and structure layouts it uses are the published interface
of that library, documented publicly at
https://gpuopen-librariesandsdks.github.io/adl/ and in AMD's ADL SDK.

## NVIDIA NVAPI / NVML

No NVIDIA code or headers are included. The tool calls `nvapi64.dll` and `nvml.dll`, both of which
ship with the NVIDIA graphics driver — NVAPI via the vendored NvAPIWrapper above, NVML through a
small P/Invoke binding in `src/GpuTuner.Core/Backends/Nvidia/Nvml.cs`.

## RadeonTuner

- **Upstream:** https://github.com/dumbie/RadeonTuner

No code from RadeonTuner is included here. It was read as a reference while working out how the
Overdrive 8 API expects to be called — specifically that `lpNumberOfFeatures` is an in/out parameter.
Credited with thanks.
