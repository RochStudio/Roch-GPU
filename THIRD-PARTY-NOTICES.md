# Third-party notices

Roch GPU itself is GPL-3.0-or-later (see `LICENSE`). It includes and depends on the following.

---

## NvAPIWrapper — vendored, modified

- **Location in this repository:** `third_party/NvAPIWrapper/`
- **Upstream:** https://github.com/falahati/NvAPIWrapper
- **Author:** Soroush Falahati
- **Licence:** GNU Lesser General Public License v3.0 — full text at
  `third_party/NvAPIWrapper/LICENSE`

This copy has been **modified**. The changes are:

1. Retargeted to `net10.0` (upstream targets an older framework).
2. Added an `InternalsVisibleTo` attribute for `GpuTuner.Core`, so the tuning code can reach the
   internal structure definitions it needs for private NVAPI calls.

The complete, modified source of the library is in this repository, and the released `RochGPU.exe`
is a single-file self-contained publish — so `NvAPIWrapper.dll` is bundled inside the executable
rather than sitting beside it, and cannot be swapped out in the shipped binary.

That bundling needs no special argument now that the application is GPL-3.0-or-later. LGPL-3.0 §2(b)
lets a recipient take a copy of the library under the plain GPL-3.0, so the combined work is simply a
GPL-3.0 work: its complete corresponding source - the application's and the modified library's alike -
is in this repository, and `build.ps1` rebuilds the whole executable from it with one command. The
library keeps its own LGPL-3.0 notice above and its licence text in `third_party/NvAPIWrapper/LICENSE`.

The corresponding source for any released binary is the repository at the tag that release was built
from; the release notes name it.

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
