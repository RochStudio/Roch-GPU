# Research: GPU thermal limit / temperature target on GeForce RTX 50 (Blackwell), Windows user mode

Date: 2026-10-17. Scope: public documentation and open-source code only. Nothing
was run against the GPU. The reference tuning utility and NVIDIA driver binaries
were not examined for this initial survey (a static look at the utility was added
later; see the addendum).

## Short answer

**There is no publicly documented, clean way to set the thermal limit on an RTX 50
card on Windows.** The only published layout that claims a working "target
temperature" setter on current drivers came from reverse engineering. Its
authors decompiled `nvapi64.dll` and an unnamed closed "reference tool", so it
breaks this project's hard rule. It was also verified only on an RTX 4060 Laptop,
not on a desktop Blackwell card.

What we can do cleanly:
1. Keep probing the documented NVML acoustic threshold (`ACOUSTIC_CURR`) and the
   classic NVAPI thermal policy calls. Show the control only when the probe
   succeeds. On the RTX 5070 Ti with driver 616.92, both are already known to be
   unavailable.
2. Build a "soft" temperature target in our own software, using documented NVML
   setters: power limit, or locked clocks / clock offsets. See "Clean-room
   options" below.

## What was searched

- NVIDIA NVAPI SDK, `github.com/NVIDIA/nvapi`, `nvapi.h` at the latest commit
  (2026-09-18). Searched for thermal, ThermalPolic, TempLimit, PerfPolic and
  PerfLimit, and listed every `NvAPI_GPU_*` thermal, cooler, power, clock and
  perf entry point.
- NVML header `nvml.h` 13.4.92 (copy in `github.com/NVIDIA/go-nvml`,
  `gen/nvml/nvml.h`). Searched every `nvmlDeviceSet*`, the temperature threshold
  enum, the `NVML_FI_DEV_*` temperature field IDs, `SetFieldValues`, and margin
  temperature.
- NVIDIA `nvidia-smi` documentation (`docs.nvidia.com/deploy/nvidia-smi/`):
  `-gtt` and `SUPPORTED_GPU_TARGET_TEMP`.
- NVIDIA Developer Forums threads on `-gtt`.
- NVIDIA `open-gpu-kernel-modules`: the RM control headers `ctrl2080thermal.h`
  and `ctrl2080perf.h`.
- Open-source tools:
  - falahati/NvAPIWrapper (FunctionId.cs)
  - arcnmx/nvapi-rs (`sys/src/nvid.rs`, `sys/src/gpu/thermal.rs`)
  - Skyworks-Neo/nvoc and its nvapi-rs fork
  - minewefu/afterglow
  - ilya-zlobintsev/LACT
  - Demion/nvapioc
  - GitHub code search for the IDs 0x34C0B13D, 0xE9C425A1, 0xE097144F and
    0xC4554575, and the names ClientThermalPolicies, ThermalPoliciesSetLimit and
    GPU_ClientThermal
- Press and forums:
  - hwcooling.net (RTX 3080 vs RTX 5080 thermals)
  - TechPowerUp, igor'sLAB and Tom's Hardware on Blackwell hotspot and MODS
  - Searches for Afterburner and Unwinder posts about the RTX 50 temp limit
- Skipped as instructed for this first sweep: HWiNFO (closed source), the reference tuning utility,
  and the NVIDIA driver binaries.

## Candidate interfaces

| # | Interface | ID / API | Read / Write | Reported to work on | Source (license) | Trust / status on RTX 5070 Ti |
|---|---|---|---|---|---|---|
| 1 | NVML acoustic threshold ("GPU Target Temperature", what `nvidia-smi -gtt` and the Linux tools use) | `nvmlDeviceSetTemperatureThreshold(dev, NVML_TEMPERATURE_THRESHOLD_ACOUSTIC_CURR=5, int*)`; range from `ACOUSTIC_MIN=4` / `ACOUSTIC_MAX=6` | R/W | Ampere and Ada. NVIDIA forum moderator shows `nvidia-smi.exe -gtt` on an RTX 4080 on Windows with range 65–88. LACT users set it on Ada and Ampere under Linux and saw SW_THERMAL_SLOWDOWN at the target. | nvml.h (NVIDIA header licence: "nonexclusive, royalty-free license to use this code in individual and commercial software"); docs.nvidia.com/deploy/nvml-api; LACT (MIT) issue #895 | **Official and documented, but unsupported on our card** (ACOUSTIC_* returns NOT_SUPPORTED). Afterglow (MIT) independently reports "Target Temperature N/A" in `nvidia-smi` on an RTX 5090 with driver 610.88. |
| 2 | `nvidia-smi -gtt / --gpu-target-temp` | Front end for #1. NVIDIA does not publicly name the NVML call behind it. That it is ACOUSTIC_CURR is our inference: the min/max output matches ACOUSTIC_MIN/MAX. | W (query with `-q -d SUPPORTED_GPU_TARGET_TEMP`) | Same as #1. Docs say "Not supported on all devices", "Requires Root". | docs.nvidia.com/deploy/nvidia-smi | Expected to fail or report N/A on RTX 50. Running it would be a GPU write, so we did not test it. |
| 3 | NVML GPU_MAX threshold write | `nvmlDeviceSetTemperatureThreshold(..., NVML_TEMPERATURE_THRESHOLD_GPU_MAX=3, ...)` | W (documented generically, "Maxwell or newer") | nvoc's `set-temp-limit --nvml` writes this. nvoc's matrix claims ✅ on RTX 50, but gives no evidence. nvoc's own comment says "Windows rejects the NVML threshold setter outright". | nvml.h; Skyworks-Neo/nvoc (Apache-2.0) | Low. Not documented as user-settable for GeForce, and contradicted by nvoc itself. If we ever probe it, treat it as read-back-verified only. |
| 4 | NVML read-only thermal info | `nvmlDeviceGetFieldValues` with `NVML_FI_DEV_TEMPERATURE_SHUTDOWN_TLIMIT=193`, `SLOWDOWN_TLIMIT=194`, `MEM_MAX_TLIMIT=195`, `GPU_MAX_TLIMIT=196` ("Ada and later"; the header says these replace `GetTemperatureThreshold`); `nvmlDeviceGetMarginTemperature` (`nvmlMarginTemperature_v1_t {u32 version; int marginTemperature;}`) | R only | Ada and later | nvml.h | Official. Useful for displaying the fixed limits and headroom. **There is no setter.** `nvmlDeviceSetFieldValues` does not exist; the only related calls are Get and Clear. |
| 5 | Classic private NVAPI thermal policies ("Temp Limit" slider in older Afterburner, NvAPIWrapper and nvapi-rs) | GetInfo `0x0D258BB5`, GetStatus `0xE9C425A1`, SetStatus `0x34C0B13D`. v2 layouts in nvapi-rs and NvAPIWrapper: info = {ver, count u8, ?, 4×{controller, ?, min, def, max, ?}}; status = {ver, count, 4×{controller, target ×256, flags}}; temps in Q8 (°C×256) | R/W | Pascal through Ada desktop | arcnmx/nvapi-rs (MIT), NvAPIWrapper (LGPL-3.0), nvapioc (no licence file), afterglow (MIT). IDs are community-published. | Widely used, but **returns no entries on our RTX 5070 Ti / 616.92**. nvoc notes the SET "returns OK on mobile but silently does not persist". Matches hwcooling.net: on RTX 50 "you can no longer adjust the temperature limit". |
| 6 | Legacy "GPS" thermal limit | `NvAPI_GPS_GetThermalLimit 0x583113ED`, `NvAPI_GPS_SetThermalLimit 0xC07E210F` | names only | Unknown. "GPS" (power steering) dates from the notebook era. NVML's `GPS_CURR` threshold is unsupported on our card. | Names only, in arcnmx/nvapi-rs (MIT) and NvAPIWrapper (LGPL-3.0) | **No published struct layout.** Using it would mean guessing a layout. Not viable. |
| 7 | Private "ClientThermalTarget" read-modify-write pair | GET `0xC4554575`, SET `0xE097144F`, plus private policies GetInfo `0x2F69F8E5`. Published layout: 984-byte buffer, version `0x203D8`, mask dword = 1<<idx, 15-dword entries, target Q8 at dword 15*idx+7; GetInfo is 15704 bytes, version `0x33D58` | R/W | Verified only on an **RTX 4060 Laptop** (GPS index 2, range 75–87, `nvidia-smi` 87→82). For desktops the authors fall back to the acoustics index, which on our card is the same NVML ACOUSTIC path that reports unsupported. No Blackwell desktop evidence. | Skyworks-Neo/nvoc (Apache-2.0) and Skyworks-Neo/nvapi-rs fork (MIT), `sys/src/gpu/thermal.rs` | **Excluded under our hard rule.** The comments say the layout was "RE'd from the ref-tool CLI (… sub_140013090)", call it "NDA", and cite `nvapi64.dll`'s "static table off_1804DD000". It was derived from decompiling NVIDIA's driver DLL and an unnamed closed tool, which might even be that reference tool. The code licences are permissive, but that does not clean the provenance. |
| 8 | Other private thermal setters listed by nvoc's audit | `ThermalHwFsSetInfo 0xCBC9361B`, `ThermChannelSetControl 0x8DF19FA2`, `SetThermalSimulationMode 0x8CD42541` / `SetExtendedThermalSimulationMode 0x95E71AB6` | W | Not published as working on any GPU. Thermal simulation spoofs the temperature reading and is dangerous. | nvoc `docs/reverse-engineering/nvapi/*.md` (Apache-2.0; RE-derived from a dump of nvapi64.dll's QueryInterface table, R610.74) | **Excluded**: same provenance problem as #7, no layouts, and unsafe. |
| 9 | Public NVAPI SDK | Only `NvAPI_GPU_GetThermalSettings` (read), `NvAPI_GPU_GetPerfDecreaseInfo` (read, has a thermal reason bit) and `NvAPI_GPU_GetOverclockStatus` (R595+, read) | R only | All | NVIDIA/nvapi, `nvapi.h` (MIT, SPDX header; the libs are also MIT per License.txt) | Official. **The public SDK has no thermal setter.** |
| 10 | RM control interface (open kernel modules) | `NV2080_CTRL_CMD_THERMAL_SYSTEM_EXECUTE_V2 0x20800513` (sensor/target queries only) | R | Linux RM | NVIDIA/open-gpu-kernel-modules, `ctrl2080thermal.h` (MIT) | Thermal *policy* controls are not in the open headers; they live in closed GSP firmware. These controls also cannot be reached from Windows user mode without undocumented driver escapes. Not viable. |

Context from the press:
- hwcooling.net found that NVIDIA removed the user-adjustable temperature limit on
  RTX 50. Only the power limit remains, with fixed maxima of 90 °C for the 5090
  and 88 °C for the 5080. NVIDIA has given no official explanation.
- TechPowerUp, igor'sLAB and Tom's Hardware report that the hotspot sensor was
  hidden from public APIs on Blackwell and is reachable only through NVIDIA's
  internal MODS tool.
- In Unwinder's public statements (TweakTown/Guru3D coverage), Afterburner is
  limited to public NVAPI for RTX 50 hotspot data.
- We found no public Afterburner or Unwinder statement documenting an RTX 50
  temp-limit API.

## Verdict

**No.** There is no publicly documented or openly published, provenance-clean
way to set the thermal limit on an RTX 50 card from Windows user mode.
- The documented paths (#1 and #2) exist, but the RTX 50 driver reports them as
  unsupported.
- The community-published classic path (#5) returns no entries on Blackwell.
- The only published Blackwell-era setter layout (#7) comes from decompiling
  NVIDIA binaries and a closed tool. It is also unverified on desktop Blackwell.

The reference tool's −40..105 °C "Thermal limit" therefore probably relies on an undocumented
interface. We must not try to rediscover it.

## Clean-room options

1. **Probe, don't assume.** At startup, read ACOUSTIC_MIN/CURR/MAX (NVML) and the
   classic ThermalPolicies GetInfo/GetStatus. Expose a "Temp limit" control only
   when a probe returns a valid range. On RTX 50 / R61x today that means the
   control is hidden, with a note such as "not exposed by the driver for this
   GPU". This uses only documented NVML calls and IDs/layouts the project already
   relies on.
2. **Read-only display of the fixed limits:**
   - NVML field IDs 193–196 for the T.Limit thresholds
   - `nvmlDeviceGetMarginTemperature` for headroom
   - `nvmlDeviceGetCurrentClocksEventReasons` with the SW thermal slowdown bit
     (0x20) and HW slowdown bit (0x8), so users can see when they hit the limit
3. **Software temperature target (fully clean).** A user-mode control loop:
   - It reads `nvmlDeviceGetTemperature(GPU)` and lowers the power limit
     (`nvmlDeviceSetPowerManagementLimit`, documented, already used) as the
     temperature passes the user's target. A gentler alternative is
     `nvmlDeviceSetGpuLockedClocks` / `nvmlDeviceSetClockOffsets`.
   - It restores the user's settings below the target, with hysteresis.
   - It must be labelled honestly as an app-side limiter, not a driver thermal
     policy.
   - Risks: the response is slower than firmware policy (poll interval and power
     limit granularity), it stops working when the app or service is not
     running, and a crash could leave a reduced power limit set. Mitigate by
     resetting to defaults on exit and in the profile-recovery path.
4. If NVIDIA later documents a setter, adopt it:
   - an NVAPI SDK release with a public `NvAPI_GPU_*Thermal*Set*` call, or
   - NVML enabling ACOUSTIC_* on Blackwell. Recheck `nvml.h` and `nvapi.h` on
     each driver branch.

## Risks if anyone revisits #7 or #8 anyway (not recommended)

- **Legal and licensing:** the information comes from decompiling NVIDIA
  binaries, against NVIDIA's licence. The "ref tool" behind it is unnamed and
  might be that reference tool. Shipping it in a GPL-3.0 project could expose the project to
  claims, and contributors cannot certify its origin.
- **Technical:** private, version-magic-checked buffers can change between
  drivers. A wrong RMW on a 984-byte or 15 KB blob could set nonsense thermal
  targets. Thermal simulation spoofs sensor readings and could disable thermal
  protection.

## Addendum 2026-10-19: what the reference utility's live limit actually calls

A reference tuning utility (a 3,437,056-byte PE) was inspected statically, without running it:

- Its only temperature-affecting setter imports are `nvmlDeviceSetGpuLockedClocks` and
  `nvmlDeviceResetGpuLockedClocks`. It contains none of the private NVAPI thermal-policy
  IDs (set `0x34C0B13D`, status `0xE9C425A1`, info `0x0D258BB5`, client target
  `0xC4554575` / `0xE097144F`) and no named `NvAPI_GPU_Set*Thermal*` import.
- Its persisted config has `thermal_limit: { enabled, temperature_raw }`, and its UI tops out
  at 105 °C.

So its live temperature limit is a clock-cap controller of the same class as ours: 105 °C means
"do not cap", not a raised hardware throttle. Roch GPU now offers the same 50…105 °C window;
the slider defaults to the card's own limit (88 °C on a 5070 Ti), and values from there up to
105 °C mean "leave it to the card", while the firmware slowdown (90 °C on the 5070 Ti) still
applies. The reference tool also has a separate VBIOS limits editor that patches a BIOS image
offline; Roch GPU deliberately does not do that.

## Sources

- NVIDIA NVAPI SDK, MIT: https://github.com/NVIDIA/nvapi (`nvapi.h`, `License.txt`)
- NVML header 13.4.92 (NVIDIA header licence; repo Apache-2.0): https://github.com/NVIDIA/go-nvml/blob/main/gen/nvml/nvml.h
- NVML API reference: https://docs.nvidia.com/deploy/nvml-api/
- nvidia-smi documentation: https://docs.nvidia.com/deploy/nvidia-smi/index.html
- NVIDIA forum "Nvidia -gtt command doesn't work" (RTX 4080 range 65–88): https://forums.developer.nvidia.com/t/nvidia-gtt-command-doesnt-work/341950
- NVIDIA forum, `-gtt` on laptops: https://forums.developer.nvidia.com/t/nvidia-smi-gtt-option-since-460-27-causes-major-performance-issues-on-laptops/176579
- hwcooling.net, RTX 50 temperature limit removed: https://www.hwcooling.net/en/geforce-rtx-3080-vs-rtx-5080-thermals-normal-or-overheating/
- LACT (MIT), issue #895, NVML ACOUSTIC_CURR target temperature: https://github.com/ilya-zlobintsev/LACT/issues/895
- LACT NVIDIA controller (MIT): https://github.com/ilya-zlobintsev/LACT/blob/master/lact-daemon/src/server/gpu_controller/nvidia.rs
- minewefu/afterglow (MIT), `docs/research/driver-apis.md` (RTX 5090 / 610.88, "Target Temperature N/A"): https://github.com/minewefu/afterglow/blob/main/docs/research/driver-apis.md
- arcnmx/nvapi-rs (MIT): https://github.com/arcnmx/nvapi-rs (`sys/src/nvid.rs`, `sys/src/gpu/thermal.rs`)
- falahati/NvAPIWrapper (LGPL-3.0): https://github.com/falahati/NvAPIWrapper/blob/master/NvAPIWrapper/Native/Helpers/FunctionId.cs
- Demion/nvapioc (no licence file): https://github.com/Demion/nvapioc
- Skyworks-Neo/nvoc (Apache-2.0), RE-derived, excluded: https://github.com/Skyworks-Neo/nvoc (`cli/README.md`, `core/src/operation.rs`, `docs/reverse-engineering/nvapi/*`)
- Skyworks-Neo/nvapi-rs fork (MIT), RE-derived, excluded: https://github.com/Skyworks-Neo/nvapi-rs/blob/v0.2.x/sys/src/gpu/thermal.rs
- NVIDIA open-gpu-kernel-modules (MIT/GPL-2.0 dual): https://github.com/NVIDIA/open-gpu-kernel-modules/blob/main/src/common/sdk/nvidia/inc/ctrl/ctrl2080/ctrl2080thermal.h
- TechPowerUp, RTX 50 hotspot via MODS: https://www.techpowerup.com/350705/nvidia-geforce-rtx-50-series-hotspot-sensor-can-be-accessed-with-nvidias-internal-tool
- TweakTown, Unwinder on RTX 50 and public NVAPI: https://www.tweaktown.com/news/112640/geforce-rtx-50-series-hotspot-temperature-readings-wont-be-added-to-msi-afterburner/index.html
