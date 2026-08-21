using GpuTuner.Core.Models;
using NvAPIWrapper;
using NvAPIWrapper.GPU;
using NvAPIWrapper.Native;
using NvAPIWrapper.Native.Exceptions;
using NvAPIWrapper.Native.GPU;
using NvAPIWrapper.Native.GPU.Structures;
using NvAPIWrapper.Native.Interfaces.GPU;

namespace GpuTuner.Core.Backends.Nvidia;

/// <summary>
/// NVIDIA backend built on NvAPIWrapper (falahati). Everything here goes through the user-mode
/// nvapi64.dll — no kernel driver. Writes require the process to be elevated.
///
/// Notes on the calls used (all "private" NVAPI, same ones Afterburner / Inspector / green-curve use):
///   Core/mem offset  → NvAPI_GPU_SetPStates20 with a delta on the Graphics/Memory clock domain of P0.
///   Power limit      → NvAPI_GPU_ClientPowerPoliciesSetStatus (value in per-cent-mille: 100000 = 100 %).
///   Temp limit       → NvAPI_GPU_SetThermalPoliciesStatus (value in °C, encoded ×256 by the wrapper).
///   Fan              → NvAPI_GPU_SetCoolerLevels (legacy) with automatic fallback to
///                      NvAPI_GPU_ClientFanCoolersSetControl (RTX 20+). The wrapper picks the right one.
/// </summary>
public sealed class NvApiBackend : IGpuBackend
{
    private PhysicalGPU[] _gpus = Array.Empty<PhysicalGPU>();
    private readonly List<GpuDevice> _devices = new();
    private readonly Dictionary<int, GpuCapabilities> _capsCache = new();
    private readonly Dictionary<int, uint> _thermalMask = new();
    private bool _initialized;

    public string BackendName => "NVIDIA (NVAPI)";
    public IReadOnlyList<GpuDevice> Devices => _devices;

    public void Initialize()
    {
        // Idempotent: BackendFactory initialises while probing, then TuningService initialises again.
        if (_devices.Count > 0) return;
        try
        {
            NVIDIA.Initialize();
        }
        catch (Exception ex)
        {
            throw new GpuBackendException(
                "NVAPI could not be initialised. Is the NVIDIA driver installed and is this a 64-bit process?", ex);
        }

        _gpus = PhysicalGPU.GetPhysicalGPUs();
        if (_gpus.Length == 0)
            throw new GpuBackendException("NVAPI initialised but reported no NVIDIA GPUs.");

        string driver = SafeGet(() => $"{NVIDIA.DriverVersion / 100}.{NVIDIA.DriverVersion % 100:00}", "unknown");

        _devices.Clear();
        for (int i = 0; i < _gpus.Length; i++)
        {
            var g = _gpus[i];
            _devices.Add(new GpuDevice(
                Index: i,
                Name: SafeGet(() => g.FullName, "NVIDIA GPU"),
                Vendor: "NVIDIA",
                BusId: SafeGet(() => $"PCI bus {g.BusInformation.BusId}, slot {g.BusInformation.BusSlot}", "PCI"),
                DriverVersion: driver,
                VramMegabytes: SafeGet(() => g.MemoryInformation.DedicatedVideoMemoryInkB / 1024L, 0L),
                BiosVersion: SafeGet(() => g.Bios.VersionString, "")));
        }
        _initialized = true;
    }

    private PhysicalGPU Gpu(int index)
    {
        if (!_initialized) throw new GpuBackendException("Backend not initialised.");
        if (index < 0 || index >= _gpus.Length) throw new ArgumentOutOfRangeException(nameof(index));
        return _gpus[index];
    }

    // ------------------------------------------------------------------ capabilities

    /// <summary>
    /// One voltage rail's ranges as the UI needs them. A rail the card does not expose leaves this
    /// at its default, where <c>Supported</c> is false and the control hides.
    /// </summary>
    private readonly record struct RailCaps(
        bool Supported, int MinMv, int MaxMv, int StockMv,
        int FloorMinMv, int FloorMaxMv, int FloorStockMv);

    public GpuCapabilities GetCapabilities(int gpuIndex)
    {
        if (_capsCache.TryGetValue(gpuIndex, out var cached)) return cached;
        var g = Gpu(gpuIndex);

        // Clock offset ranges from PStates20 (P0).
        bool canCore = false, canMem = false;
        int coreMin = -500, coreMax = 500, memMin = -1000, memMax = 1500;
        try
        {
            var ps = GPUApi.GetPerformanceStates20(g.Handle);
            if (ps.Clocks.TryGetValue(PerformanceStateId.P0_3DPerformance, out var clocks))
            {
                foreach (var c in clocks)
                {
                    var range = c.FrequencyDeltaInkHz.DeltaRange;
                    if (c.DomainId == PublicClockDomain.Graphics)
                    {
                        canCore = ps.IsEditable && c.IsEditable;
                        coreMin = range.Minimum / 1000; coreMax = range.Maximum / 1000;
                    }
                    else if (c.DomainId == PublicClockDomain.Memory)
                    {
                        canMem = ps.IsEditable && c.IsEditable;
                        // Take the driver's own maximum. This used to be forced up to +4000 on the
                        // theory that 50-series cards under-report what they'll accept; a 5070 Ti
                        // (driver 610.88) reports +3000 and rejects anything above it outright with
                        // NotSupported rather than clamping to the limit, so the extra travel was
                        // slider the card could only ever refuse.
                        memMin = range.Minimum / 1000; memMax = range.Maximum / 1000;
                    }
                }
            }
        }
        catch (NVIDIAApiException) { /* leave defaults */ }
        catch (NVIDIANotSupportedException) { }

        // Power limit.
        bool canPower = false; int pMin = 100, pMax = 100, pDef = 100;
        try
        {
            var info = g.PerformanceControl.PowerLimitInformation.FirstOrDefault();
            if (info != null)
            {
                canPower = true;                       // a policy entry exists, so the limit is settable
                pMin = (int)Math.Round(info.MinimumPowerInPercent);
                pMax = (int)Math.Round(info.MaximumPowerInPercent);
                pDef = (int)Math.Round(info.DefaultPowerInPercent);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // Temp limit. Zeros when the card exposes no thermal policy — a range of 65..90 with a
        // default of 83 is a description of some other card, and everything downstream (the stock
        // profile, the clamp) would carry it as though this one had a limit.
        bool canTemp = false; int tMin = 0, tMax = 0, tDef = 0;
        try
        {
            var info = g.PerformanceControl.ThermalLimitInformation.FirstOrDefault();
            if (info != null)
            {
                canTemp = true;
                tMin = info.MinimumTemperature; tMax = info.MaximumTemperature; tDef = info.DefaultTemperature;
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // Fans.
        bool canFan = false; int fanCount = 0, fMin = 0, fMax = 100;
        try
        {
            var coolers = g.CoolerInformation.Coolers.ToList();
            fanCount = coolers.Count;
            canFan = fanCount > 0;
            if (fanCount > 0)
            {
                fMin = coolers.Min(c => c.DefaultMinimumLevel);
                fMax = coolers.Max(c => c.DefaultMaximumLevel);
                if (fMax <= 0) fMax = 100;
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        bool canVolt = false;
        try { _ = GPUApi.GetCurrentVoltage(g.Handle); canVolt = true; } catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // Voltage boost (the "overvoltage" percent Afterburner exposes). Read the current value to probe support.
        bool canVoltBoost = false;
        try { _ = GPUApi.GetCoreVoltageBoostPercent(g.Handle); canVoltBoost = true; } catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // V/F curve editing — the real undervolt path. Needs both the curve read and the delta table.
        // Probe each call separately so a failure says which one and why, instead of a silent "unavailable".
        bool canCurve = false; int stockMaxMv = 0; string curveReason = "";
        try
        {
            if (!NvApiPrivate.CurveAvailable)
            {
                curveReason = "the driver does not export the V/F curve entry points";
            }
            else
            {
                var basePts = ReadBaseCurve(g);
                uint maxUv = VfCurve.MaxVoltageUv(basePts);
                if (maxUv > 0) { canCurve = true; stockMaxMv = (int)(maxUv / 1000); }
                else curveReason = $"driver returned an empty curve (mask 0x{CurveMask(g)[0]:X8})";
            }
        }
        catch (NVIDIAApiException ex) { curveReason = $"NVAPI {ex.Status}"; }
        catch (NVIDIANotSupportedException) { curveReason = "entry point not exposed by this driver"; }
        catch (Exception ex) { curveReason = ex.GetType().Name + ": " + ex.Message; }

        // Core voltage rail (NVVDD). Read rather than assumed: the rail reports its own ceiling and
        // floor, and the ceiling already includes whatever offset is applied, so stock is the
        // difference. A card without the rail family simply reports nothing and the control hides.
        var rails = new Dictionary<int, RailCaps>();
        try
        {
            foreach (var rail in NvApiPrivate.ReadVoltRails(g.Handle))
            {
                if (rail.MaxUv == 0) continue;
                // Two different numbers, and conflating them was a bug worth naming. The reported
                // ceiling is what the rail is CONFIGURED at; subtracting the offset gives the base
                // the hardware counts from. MSVDD ships with a -50 mV offset already applied, so its
                // configured ceiling is 985 mV against a 1035 mV base — the same base as NVVDD.
                // Calling the base "stock" made the UI claim 1035, and made Reset write offset 0,
                // which raised the rail 50 mV ABOVE where the card left the factory.
                int configured = (int)rail.MaxUv / 1000;
                int hwBase = ((int)rail.MaxUv - rail.MaxOffsetUv) / 1000;
                int min = (int)rail.MinUv / 1000;
                int max = hwBase + VoltageRailHeadroomMv;
                // The floor reads the same way: the reported minimum already carries its own offset,
                // so the hardware's base floor is the difference. Travel is capped at the same
                // headroom as the ceiling — enough for stability work, and short of pinning the rail
                // near its top where the card would sit at high voltage even at idle.
                int floorBase = ((int)rail.MinUv - rail.MinOffsetUv) / 1000;
                rails[rail.Index] = new RailCaps(
                    hwBase > 0 && max > min, min, max, configured,
                    floorBase, floorBase + VoltageRailHeadroomMv, min);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // A rail the card doesn't have leaves a default RailCaps, whose Supported is false — which is
        // exactly what the UI needs to hide the control.
        var nvvdd = rails.GetValueOrDefault(NvApiPrivate.RailNvvdd);
        var msvdd = rails.GetValueOrDefault(NvApiPrivate.RailMsvdd);

        // Crossbar. Its own control family reports the range; no practical narrowing, because it is a
        // clock rather than a voltage — an offset that is too high shows up as instability, not damage.
        bool canXbar = false; int xbarMin = 0, xbarMax = 0;
        try
        {
            var xi = NvApiPrivate.ReadXbarInfo(g.Handle);
            if (xi.Supported)
            {
                canXbar = true;
                (xbarMin, xbarMax) = ClockStep.Narrow(xi.MinOffsetMhz, xi.MaxOffsetMhz,
                    ClockStep.XbarOffsetPracticalMinMhz, ClockStep.XbarOffsetPracticalMaxMhz);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // SYS and video sit in the same info list as the crossbar, in their own slots. A slot that
        // reports an offset range is one the driver will discuss; the practical narrowing is the
        // crossbar's, because the reasoning is the same — the +/-1000 is the delta field's width
        // rather than a claim about the domain.
        bool canSys = false, canVideo = false;
        int sysMin = 0, sysMax = 0, vidMin = 0, vidMax = 0;
        try
        {
            foreach (var d in NvApiPrivate.ReadDomainEntries(g.Handle))
            {
                if (!d.HasRange) continue;
                if (d.Index == NvApiPrivate.SlotSys)
                {
                    canSys = true;
                    (sysMin, sysMax) = ClockStep.Narrow(d.MinMhz, d.MaxMhz,
                        ClockStep.XbarOffsetPracticalMinMhz, ClockStep.XbarOffsetPracticalMaxMhz);
                }
                else if (d.Index == NvApiPrivate.SlotVideo)
                {
                    canVideo = true;
                    (vidMin, vidMax) = ClockStep.Narrow(d.MinMhz, d.MaxMhz,
                        ClockStep.XbarOffsetPracticalMinMhz, ClockStep.XbarOffsetPracticalMaxMhz);
                }
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        var coreRange = ClockStep.Narrow(coreMin, coreMax,
            ClockStep.CoreOffsetPracticalMinMhz, ClockStep.CoreOffsetPracticalMaxMhz);
        var memRange = ClockStep.Narrow(memMin, memMax,
            ClockStep.MemoryOffsetPracticalMinMhz, ClockStep.MemoryOffsetPracticalMaxMhz);

        var caps = new GpuCapabilities
        {
            CanSetCoreOffset = canCore, CanSetMemoryOffset = canMem,
            CanSetPowerLimit = canPower, CanSetTempLimit = canTemp,
            CanSetFanSpeed = canFan, CanReadVoltage = canVolt,
            CanSetVoltageBoost = canVoltBoost,
            CanSetVoltageCurve = canCurve, StockMaxVoltageMv = stockMaxMv, CurveUnavailableReason = curveReason,
            // Depth is measured down from the curve's own top, so cap it relative to that rather
            // than a flat -300 mV which could drop below the bottom of the curve entirely.
            VoltageOffsetMinMv = stockMaxMv > 0 ? -Math.Clamp(stockMaxMv - 700, 50, 300) : -300,
            VoltageOffsetMaxMv = 0,
            MinVoltageMv = 850,
            MaxVoltageMv = stockMaxMv > 0 ? stockMaxMv + VoltageBoostHeadroomMv : 0,
            // Driver-reported travel, narrowed to what is actually tunable — see ClockStep.
            CoreOffsetMinMhz = coreRange.min, CoreOffsetMaxMhz = coreRange.max,
            MemoryOffsetMinMhz = memRange.min, MemoryOffsetMaxMhz = memRange.max,
            PowerLimitMinPercent = pMin, PowerLimitMaxPercent = pMax, PowerLimitDefaultPercent = pDef,
            TempLimitMinC = tMin, TempLimitMaxC = tMax, TempLimitDefaultC = tDef,
            VoltageBoostMinPercent = 0, VoltageBoostMaxPercent = 100,
            FanMinPercent = fMin, FanMaxPercent = fMax, FanCount = fanCount,
            CanSetVoltageRail = nvvdd.Supported,
            VoltageRailMinMv = nvvdd.MinMv, VoltageRailMaxMv = nvvdd.MaxMv, VoltageRailStockMaxMv = nvvdd.StockMv,
            VoltageRailFloorMinMv = nvvdd.FloorMinMv, VoltageRailFloorMaxMv = nvvdd.FloorMaxMv, VoltageRailStockFloorMv = nvvdd.FloorStockMv,
            CanSetMsvddRail = msvdd.Supported,
            MsvddRailMinMv = msvdd.MinMv, MsvddRailMaxMv = msvdd.MaxMv, MsvddRailStockMaxMv = msvdd.StockMv,
            MsvddRailFloorMinMv = msvdd.FloorMinMv, MsvddRailFloorMaxMv = msvdd.FloorMaxMv, MsvddRailStockFloorMv = msvdd.FloorStockMv,
            CanSetXbarOffset = canXbar, XbarOffsetMinMhz = xbarMin, XbarOffsetMaxMhz = xbarMax,
            CanSetSysOffset = canSys, SysOffsetMinMhz = sysMin, SysOffsetMaxMhz = sysMax,
            CanSetVideoOffset = canVideo, VideoOffsetMinMhz = vidMin, VideoOffsetMaxMhz = vidMax
        };
        _capsCache[gpuIndex] = caps;
        return caps;
    }

    // ------------------------------------------------------------------ telemetry

    /// <summary>
    /// The public thermal-sensor call and nothing else — measured at 0.03 ms against 12.9 ms for the
    /// full read, of which the power-topology query alone is 8.8 ms. Used for background ticks that
    /// only feed the fan curve.
    /// </summary>
    public GpuTelemetry ReadTemperatureOnly(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        double temp = 0;
        try
        {
            foreach (var s in g.ThermalInformation.ThermalSensors)
                if (s.Target == ThermalSettingsTarget.GPU) temp = s.CurrentTemperature;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
        return new GpuTelemetry { TemperatureC = temp };
    }

    public GpuTelemetry ReadTelemetry(int gpuIndex)
    {
        var g = Gpu(gpuIndex);

        double core = 0, mem = 0;
        try
        {
            var clocks = g.CurrentClockFrequencies;
            core = clocks.GraphicsClock.Frequency / 1000.0;
            mem = clocks.MemoryClock.Frequency / 1000.0;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        double temp = 0, hotspot = double.NaN, memTemp = double.NaN;
        try
        {
            foreach (var s in g.ThermalInformation.ThermalSensors)
            {
                if (s.Target == ThermalSettingsTarget.GPU) temp = s.CurrentTemperature;
                else if (s.Target == ThermalSettingsTarget.Memory) memTemp = s.CurrentTemperature;
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // Hot-spot and memory-junction come from the private thermal-sensors call (same as HWiNFO/LHM).
        try
        {
            if (!_thermalMask.TryGetValue(gpuIndex, out var mask))
            {
                mask = NvApiPrivate.ProbeMask(g.Handle);
                _thermalMask[gpuIndex] = mask;
            }
            if (mask != 0)
            {
                var slots = NvApiPrivate.Read(g.Handle, mask);
                var (hs, mj) = NvApiPrivate.Interpret(_devices[gpuIndex].Name, slots);
                if (!double.IsNaN(hs)) hotspot = hs;
                if (!double.IsNaN(mj)) memTemp = mj;
            }
        }
        catch { /* private call best-effort */ }

        double voltage = double.NaN;
        try { voltage = GPUApi.GetCurrentVoltage(g.Handle).ValueInMicroVolt / 1000.0; } catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        double power = 0;
        try
        {
            var entries = g.PowerTopologyInformation.PowerTopologyEntries.ToList();
            var gpuEntry = entries.FirstOrDefault(e => e.Domain == PowerTopologyDomain.GPU) ?? entries.FirstOrDefault();
            if (gpuEntry != null) power = gpuEntry.PowerUsageInPercent;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        double load = 0, memLoad = 0;
        try
        {
            load = g.UsageInformation.GPU.Percentage;
            memLoad = g.UsageInformation.FrameBuffer.Percentage;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        double memUsed = 0;
        try
        {
            var mi = g.MemoryInformation;
            memUsed = (mi.AvailableDedicatedVideoMemoryInkB - mi.CurrentAvailableDedicatedVideoMemoryInkB) / 1024.0;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        var fanPcts = new List<double>(); var fanRpms = new List<double>();
        try
        {
            foreach (var c in g.CoolerInformation.Coolers)
            {
                fanPcts.Add(c.CurrentLevel);
                fanRpms.Add(c.CurrentFanSpeedInRPM);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        string pstate = "";
        try { pstate = GPUApi.GetCurrentPerformanceState(g.Handle).ToString().Split('_')[0]; } catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        string limit = "";
        try
        {
            var l = g.PerformanceControl.CurrentActiveLimit;
            limit = l == PerformanceLimit.None ? "None" : l.ToString().Replace("Limit", "");
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        return new GpuTelemetry
        {
            CoreClockMhz = core, MemoryClockMhz = mem,
            TemperatureC = temp, HotSpotC = hotspot, MemoryTemperatureC = memTemp,
            VoltageMv = voltage, PowerPercent = power,
            GpuLoadPercent = load, MemoryLoadPercent = memLoad, MemoryUsedMb = memUsed,
            FanPercent = fanPcts.Count > 0 ? fanPcts.Max() : 0,
            FanRpm = fanRpms.Count > 0 ? fanRpms.Max() : 0,
            FanPercents = fanPcts.ToArray(), FanRpms = fanRpms.ToArray(),
            PerfState = pstate, LimitReason = limit
        };
    }

    public GpuTuningState ReadTuningState(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        int core = 0, mem = 0;
        try
        {
            var ps = GPUApi.GetPerformanceStates20(g.Handle);
            if (ps.Clocks.TryGetValue(PerformanceStateId.P0_3DPerformance, out var clocks))
            {
                foreach (var c in clocks)
                {
                    if (c.DomainId == PublicClockDomain.Graphics) core = c.FrequencyDeltaInkHz.DeltaValue / 1000;
                    else if (c.DomainId == PublicClockDomain.Memory) mem = c.FrequencyDeltaInkHz.DeltaValue / 1000;
                }
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        int power = 100;
        try
        {
            var p = g.PerformanceControl.PowerLimitPolicies.FirstOrDefault();
            if (p != null) power = (int)Math.Round(p.PowerTargetInPercent);
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // 0, not a plausible-looking 83: a card with no thermal policy has no temperature limit to
        // report, and inventing one made `info` print "temp 83°C" two lines under "Temp limit: not
        // supported". Callers treat 0 as "the card didn't say".
        int temp = 0;
        try
        {
            var t = g.PerformanceControl.ThermalLimitPolicies.FirstOrDefault();
            if (t != null) temp = t.TargetTemperature;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        int voltBoost = 0;
        try { voltBoost = (int)GPUApi.GetCoreVoltageBoostPercent(g.Handle).Percent; } catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // Infer the voltage offset from the live curve: if the top is flattened, report how far
        // below the stock maximum that plateau starts.
        int voltOffset = 0;
        try
        {
            int lockMv = ReadVoltageLockMv(gpuIndex);
            if (lockMv > 0)
            {
                uint stockMax = VfCurve.MaxVoltageUv(ReadBaseCurve(g));
                if (stockMax > 0) voltOffset = lockMv - (int)(stockMax / 1000);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        try
        {
            // Only worth inferring from the curve when the lock did not already answer.
            if (voltOffset != 0) goto afterCurveInference;
            var eff = ReadEffectiveCurve(g);
            uint capUv = VfCurve.InferCapVoltageUv(eff);
            if (capUv > 0)
            {
                uint stockMax = VfCurve.MaxVoltageUv(ReadBaseCurve(g));
                if (stockMax > capUv) voltOffset = -(int)((stockMax - capUv) / 1000);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
        afterCurveInference:

        bool fanManual = false; int fanPct = 0;
        try
        {
            var coolers = g.CoolerInformation.Coolers.ToList();
            if (coolers.Count > 0)
            {
                fanManual = coolers.Any(c => c.CurrentPolicy == CoolerPolicy.Manual);
                fanPct = coolers.Max(c => c.CurrentLevel);
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        // One pass for both rails. Reading them separately cost three extra driver calls and ~16 KB
        // of marshalling per tuning-state read, on a path the GUI hits after every Apply.
        int railMaxMv = 0, msvddMaxMv = 0, railFloorMv = 0, msvddFloorMv = 0;
        try
        {
            foreach (var rail in NvApiPrivate.ReadVoltRails(g.Handle))
            {
                if (rail.Index == NvApiPrivate.RailNvvdd) { railMaxMv = (int)rail.MaxUv / 1000; railFloorMv = (int)rail.MinUv / 1000; }
                else if (rail.Index == NvApiPrivate.RailMsvdd) { msvddMaxMv = (int)rail.MaxUv / 1000; msvddFloorMv = (int)rail.MinUv / 1000; }
            }
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        int xbarMhz = 0, sysMhz = 0, videoMhz = 0;
        try
        {
            sysMhz = NvApiPrivate.ReadDomainOffsetMhz(g.Handle, NvApiPrivate.SlotSys);
            videoMhz = NvApiPrivate.ReadDomainOffsetMhz(g.Handle, NvApiPrivate.SlotVideo);
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
        try { xbarMhz = NvApiPrivate.ReadXbarOffsetMhz(g.Handle); }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        return new GpuTuningState
        {
            CoreOffsetMhz = core, MemoryOffsetMhz = mem,
            VoltageRailMaxMv = railMaxMv, MsvddRailMaxMv = msvddMaxMv,
            VoltageRailFloorMv = railFloorMv, MsvddRailFloorMv = msvddFloorMv,
            XbarOffsetMhz = xbarMhz, SysOffsetMhz = sysMhz, VideoOffsetMhz = videoMhz,
            PowerLimitPercent = power, TempLimitC = temp,
            VoltageBoostPercent = voltBoost, VoltageOffsetMv = voltOffset,
            FanManual = fanManual, FanPercent = fanPct
        };
    }

    // ------------------------------------------------------------------ writes

    /// <summary>
    /// Move the core rail's ceiling to an absolute mV target. 0 restores the card's own default.
    /// The rail takes a signed offset rather than an absolute, so the stock ceiling is recovered
    /// from the live reading first — its reported maximum already carries any offset in force.
    /// </summary>
    public void SetVoltageRailMax(int gpuIndex, int mv) => SetRailEnd(gpuIndex, NvApiPrivate.RailNvvdd, mv, Nvvdd, End.Ceiling);
    public void SetMsvddRailMax(int gpuIndex, int mv) => SetRailEnd(gpuIndex, NvApiPrivate.RailMsvdd, mv, Msvdd, End.Ceiling);
    public void SetVoltageRailFloor(int gpuIndex, int mv) => SetRailEnd(gpuIndex, NvApiPrivate.RailNvvdd, mv, Nvvdd, End.Floor);
    public void SetMsvddRailFloor(int gpuIndex, int mv) => SetRailEnd(gpuIndex, NvApiPrivate.RailMsvdd, mv, Msvdd, End.Floor);

    private const string Nvvdd = "core (NVVDD)", Msvdd = "MSVDD";
    private enum End { Floor, Ceiling }

    /// <summary>
    /// Move one end of a voltage rail to an absolute mV target. Both ends work identically: the
    /// driver reports the end it is currently configured at, offset included, so the base the
    /// hardware counts from is that minus the offset and the write is a new offset from the base.
    /// </summary>
    private void SetRailEnd(int gpuIndex, int railIndex, int millivolts, string name, End end)
    {
        bool floor = end == End.Floor;
        var g = Gpu(gpuIndex);
        int baseUv = 0;
        foreach (var rail in NvApiPrivate.ReadVoltRails(g.Handle))
            if (rail.Index == railIndex)
            {
                baseUv = floor ? (int)rail.MinUv - rail.MinOffsetUv : (int)rail.MaxUv - rail.MaxOffsetUv;
                break;
            }
        if (baseUv <= 0) throw new GpuBackendException($"This card does not expose a {name} voltage rail.");

        int offsetUv = millivolts <= 0 ? 0 : millivolts * 1000 - baseUv;
        int status = floor
            ? NvApiPrivate.WriteVoltRailMinOffset(g.Handle, railIndex, offsetUv)
            : NvApiPrivate.WriteVoltRailMaxOffset(g.Handle, railIndex, offsetUv);
        if (status != 0)
            throw new GpuBackendException(
                $"Failed to set the {name} rail {(floor ? "floor" : "ceiling")} to {millivolts} mV " +
                $"(offset {offsetUv / 1000:+#;-#;0} mV): status {status}.");
    }

    /// <summary>
    /// Offset the crossbar clock. Verified against the GPU's own frequency counter rather than the
    /// call's return: a +100 MHz offset moved the measured crossbar 2414 -> 2514 MHz with the core
    /// clock unchanged, which is what distinguishes this from a number the driver merely stored.
    /// </summary>
    /// <summary>
    /// Every clock domain this family lists, with the offset range it admits to and what its own
    /// counter currently measures. The domains that carry a range are the ones worth offering.
    /// </summary>
    public IReadOnlyList<(int Slot, int Type, bool HasRange, int MinMhz, int MaxMhz, int MeasuredMhz, int OffsetMhz)>
        ReadClockDomains(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        var list = new List<(int, int, bool, int, int, int, int)>();
        foreach (var d in NvApiPrivate.ReadDomainEntries(g.Handle))
            list.Add((d.Index, d.Type, d.HasRange, d.MinMhz, d.MaxMhz,
                      (int)(NvApiPrivate.MeasureClockKhz(g.Handle, d.Type) / 1000),
                      NvApiPrivate.ReadDomainOffsetMhz(g.Handle, d.Index)));
        return list;
    }

    /// <summary>
    /// Offset one clock domain by slot. Returns the driver's status: 0 landed, anything else was
    /// refused. Not thrown on, because "this domain reports a range and will not move" is a real
    /// answer that the caller needs to be able to report rather than treat as a fault.
    /// </summary>
    public int SetClockDomainOffset(int gpuIndex, int slot, int offsetMhz) =>
        NvApiPrivate.WriteDomainOffsetMhz(Gpu(gpuIndex).Handle, slot, offsetMhz);

    public void SetSysOffset(int gpuIndex, int offsetMhz) =>
        SetDomain(gpuIndex, NvApiPrivate.SlotSys, offsetMhz, "SYS");

    public void SetVideoOffset(int gpuIndex, int offsetMhz) =>
        SetDomain(gpuIndex, NvApiPrivate.SlotVideo, offsetMhz, "video");

    /// <summary>As <see cref="SetXbarOffset"/>, for the other domains in the same family.</summary>
    private void SetDomain(int gpuIndex, int slot, int offsetMhz, string name)
    {
        int status = NvApiPrivate.WriteDomainOffsetMhz(Gpu(gpuIndex).Handle, slot, offsetMhz);
        if (status == 0) return;
        throw new GpuBackendException(
            $"Failed to set the {name} clock offset to {offsetMhz:+#;-#;0} MHz: status {status}." +
            (status == -1 ? " The request was accepted and the value refused." : ""));
    }

    public void SetXbarOffset(int gpuIndex, int offsetMhz)
    {
        var g = Gpu(gpuIndex);
        int status = NvApiPrivate.WriteXbarOffsetMhz(g.Handle, offsetMhz);
        if (status == 0) return;

        // -1 is the driver refusing the value, not the request: a wrong struct shape answers -9, and
        // every other part of this family checks out on a card that still refuses to move. Say which
        // it is, because "status -1" alone sends the next person looking for a layout bug that is not
        // there. See the note on ProbeXbarControlShapes.
        string why = status == -1
            ? " The request was accepted and the value refused, which is what a card that reports a " +
              "crossbar range but has no controllable crossbar domain does. Reads and 0 still work."
            : "";
        throw new GpuBackendException(
            $"Failed to set the crossbar offset to {offsetMhz:+#;-#;0} MHz: status {status}.{why}");
    }

    public void SetCoreOffset(int gpuIndex, int offsetMhz) => SetClockDelta(gpuIndex, PublicClockDomain.Graphics, offsetMhz);
    public void SetMemoryOffset(int gpuIndex, int offsetMhz) => SetClockDelta(gpuIndex, PublicClockDomain.Memory, offsetMhz);

    private void SetClockDelta(int gpuIndex, PublicClockDomain domain, int offsetMhz)
    {
        var g = Gpu(gpuIndex);
        try
        {
            // Build a minimal PSTATES20 V1 payload: 1 pstate (P0), 1 clock entry, 0 voltages.
            var clockEntry = new PerformanceStates20ClockEntryV1(
                domain,
                new PerformanceStates20ParameterDelta(offsetMhz * 1000));

            var pstate = new PerformanceStates20InfoV1.PerformanceState20(
                PerformanceStateId.P0_3DPerformance,
                new[] { clockEntry },
                Array.Empty<PerformanceStates20BaseVoltageEntryV1>());

            var info = new PerformanceStates20InfoV1(new[] { pstate }, clocksCount: 1, baseVoltagesCount: 0);
            GPUApi.SetPerformanceStates20(g.Handle, info);
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to set {domain} clock offset to {offsetMhz:+#;-#;0} MHz: {ex.Status}. " +
                                          "Run as administrator and make sure no other OC tool is fighting for control.", ex);
        }
    }

    // Which power-limit payload shape this driver/card accepts. Cached after the first success
    // so we don't re-probe on every apply. -1 = not yet determined.
    private int _powerStrategy = -1;

    public void SetPowerLimit(int gpuIndex, int percent)
    {
        var g = Gpu(gpuIndex);

        // Clamp to the PCM range the driver itself reported, rather than trusting percent*1000.
        // Some cards report a min/max that doesn't land on a round percentage.
        uint pcm = (uint)(percent * 1000);
        try
        {
            var info = g.PerformanceControl.PowerLimitInformation.FirstOrDefault();
            if (info != null) pcm = Math.Clamp(pcm, info.MinimumPowerInPCM, info.MaximumPowerInPCM);
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        PrivatePowerPoliciesStatusV1.PowerPolicyStatusEntry[] current;
        try { current = GPUApi.ClientPowerPoliciesGetStatus(g.Handle).PowerPolicyStatusEntries; }
        catch (NVIDIAApiException ex) { throw new GpuBackendException($"Cannot read power policy: {ex.Status}.", ex); }
        if (current.Length == 0) throw new GpuBackendException("GPU reports no power policy entries.");

        // Payload shapes seen in the wild. Different driver branches accept different ones.
        Action[] strategies =
        {
            // 0: one fresh entry — pstate 0, unknown fields zeroed. What nvapioc / Afterburner send.
            () => GPUApi.ClientPowerPoliciesSetStatus(g.Handle, new PrivatePowerPoliciesStatusV1(
                new[] { new PrivatePowerPoliciesStatusV1.PowerPolicyStatusEntry(pcm) })),

            // 1: copy the driver's own entry 0, rewrite only the target (keeps pstate + unknown fields).
            () =>
            {
                var e = current[0];
                e._PowerTargetInPCM = pcm;
                GPUApi.ClientPowerPoliciesSetStatus(g.Handle, new PrivatePowerPoliciesStatusV1(new[] { e }));
            },

            // 2: send every entry back, with the target rewritten in all of them.
            () =>
            {
                var all = current.Select(e => { e._PowerTargetInPCM = pcm; return e; }).ToArray();
                GPUApi.ClientPowerPoliciesSetStatus(g.Handle, new PrivatePowerPoliciesStatusV1(all));
            },

            // 3: fresh entry pinned to P0 with the leading unknown set to 1 (some branches treat it
            //    as a "this field is valid" flag rather than padding).
            () =>
            {
                var e = new PrivatePowerPoliciesStatusV1.PowerPolicyStatusEntry(pcm);
                e._PerformanceStateId = PerformanceStateId.P0_3DPerformance;
                e._Unknown1 = 1;
                GPUApi.ClientPowerPoliciesSetStatus(g.Handle, new PrivatePowerPoliciesStatusV1(new[] { e }));
            },
        };

        // If we already know which shape works, use it directly.
        if (_powerStrategy >= 0 && _powerStrategy < strategies.Length)
        {
            try { strategies[_powerStrategy](); return; }
            catch (NVIDIAApiException) { _powerStrategy = -1; }   // driver changed its mind; re-probe
        }

        var failures = new List<string>();
        for (int i = 0; i < strategies.Length; i++)
        {
            try { strategies[i](); _powerStrategy = i; return; }
            catch (NVIDIAApiException ex) { failures.Add($"#{i}:{ex.Status}"); }
        }

        throw new GpuBackendException(
            $"Failed to set power limit to {percent}% ({pcm} PCM) — the driver rejected every payload shape " +
            $"({string.Join(", ", failures)}). Run 'gputuner-cli diag' and send the output.");
    }

    // ---- V/F curve helpers ------------------------------------------------

    /// <summary>
    /// The mask telling the driver which curve points to fill. NvAPIWrapper leaves it at zero, which
    /// makes the driver return Ok with an entirely empty curve, so we resolve a real one and reuse it.
    /// </summary>
    private uint[]? _curveMask;

    private uint[] CurveMask(PhysicalGPU g)
    {
        if (_curveMask != null) return _curveMask;

        // Preferred: ask the driver which points exist — GetClockBoostMask fills its own mask field.
        try
        {
            var mask = GPUApi.GetClockBoostMask(g.Handle)._Masks;
            if (mask is { Length: 4 } && mask.Any(x => x != 0)
                && NvApiPrivate.ReadCurve(g.Handle, mask).Any(p => p.voltUv > 0))
                return _curveMask = mask;
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }

        return _curveMask = NvApiPrivate.FullMask;   // fallback: request every point
    }

    /// <summary>
    /// How many GPU curve points this driver's struct actually carries, and which struct version.
    /// NvAPIWrapper hardcodes 80, which truncates cards whose curve runs higher — a 4070 Ti stops at
    /// 945 mV while really boosting near 1095 mV. Probe once for the widest layout the driver accepts.
    /// </summary>
    /// <summary>
    /// How much extra core voltage 100 % boost buys. Measured at ~60 mV on a 4070 Ti
    /// (1090 mV stock curve top → 1150 mV observed). NVAPI exposes no way to query it.
    /// </summary>
    private const int VoltageBoostHeadroomMv = 60;

    /// <summary>
    /// How far above its stock ceiling a voltage rail may be driven from here, in mV.
    ///
    /// The driver accepts far more: this card took the ceiling to 1280 mV against a 1035 mV stock,
    /// +245 mV, before clamping. That is not a number anyone should arrive at by dragging a slider.
    /// The reasoning is the same as the practical clock ranges in ClockStep — offer the part of the
    /// travel worth using, and leave the rest to someone who edits the source deliberately.
    /// </summary>
    private const int VoltageRailHeadroomMv = 115;

    private (int entries, int version)? _curveLayout;

    private (int entries, int version) CurveLayout(PhysicalGPU g)
    {
        if (_curveLayout != null) return _curveLayout.Value;

        var mask = CurveMask(g);
        var best = (entries: 80, version: 1);
        int bestPoints = 0, bestMv = 0;

        foreach (int entries in new[] { 80, 103, 128, 160, 255 })
        {
            foreach (int ver in new[] { 1, 2, 3 })
            {
                var (status, pts) = NvApiPrivate.ReadCurveRaw(g.Handle, mask, entries, ver);
                if (status != 0 || pts.Length == 0) continue;

                // A layout only wins if it reaches higher up the curve, and its delta table must
                // round-trip too — otherwise we could read points we can never write back.
                int maxMv = pts.Max(p => p.mv);
                if (maxMv <= bestMv && pts.Length <= bestPoints) continue;
                var dl = NvApiPrivate.ReadDeltasRaw(g.Handle, mask, entries, ver);
                if (dl.status != 0 || dl.deltas.Length < pts.Length) continue;

                best = (entries, ver); bestPoints = pts.Length; bestMv = maxMv;
            }
        }
        return (_curveLayout = best).Value;
    }

    /// <summary>Live curve, read with the widest layout the driver accepts.</summary>
    private List<VfPoint> ReadEffectiveCurve(PhysicalGPU g)
    {
        var (entries, ver) = CurveLayout(g);
        var (status, pts) = NvApiPrivate.ReadCurveRaw(g.Handle, CurveMask(g), entries, ver);
        if (status == 0 && pts.Length > 0)
            return pts.Select(p => new VfPoint((uint)(p.mv * 1000), p.mhz * 1000)).ToList();

        var raw = NvApiPrivate.ReadCurve(g.Handle, CurveMask(g));
        if (raw.Any(p => p.voltUv > 0 && p.freqKhz > 0))
            return raw.Where(p => p.voltUv > 0 && p.freqKhz > 0)
                      .Select(p => new VfPoint(p.voltUv, p.freqKhz)).ToList();

        // Last resort, through NvAPIWrapper. Both regions again: its "MemoryCurveEntries" carries the
        // tail of the GPU curve on cards with more than 80 points, so reading only the first array
        // would hand back a curve that stops 145 mV short on a 4070 Ti.
        var wrapped = GPUApi.GetVFPCurve(g.Handle);
        return wrapped.GPUCurveEntries.Concat(wrapped.MemoryCurveEntries)
            .Where(e => e.VoltageInMicroV > 0 && e.FrequencyInkHz > 0)
            .Select(e => new VfPoint(e.VoltageInMicroV, (int)e.FrequencyInkHz))
            .ToList();
    }

    private int[] ReadCurveDeltas(PhysicalGPU g, int count)
    {
        var (entries, ver) = CurveLayout(g);
        var (status, d) = NvApiPrivate.ReadDeltasRaw(g.Handle, CurveMask(g), entries, ver);
        if (status == 0 && d.Length > 0) return d;

        var legacy = NvApiPrivate.ReadDeltas(g.Handle, CurveMask(g));
        if (legacy.Length > 0) return legacy;
        // NVIDIANotSupportedException derives from NotSupportedException, NOT from NVIDIAApiException:
        // DelegateFactory throws it when the driver doesn't export this private entry point at all.
        try { return GPUApi.GetClockBoostTable(g.Handle).GPUDeltas.Select(x => x.FrequencyDeltaInkHz).ToArray(); }
        catch (NVIDIAApiException) { return new int[count]; }
        catch (NVIDIANotSupportedException) { return new int[count]; }
    }

    /// <summary>Read the stock curve: the live curve with any currently-applied deltas subtracted out.</summary>
    private List<VfPoint> ReadBaseCurve(PhysicalGPU g)
    {
        var curve = ReadEffectiveCurve(g);
        var deltas = ReadCurveDeltas(g, curve.Count);
        var pts = new List<VfPoint>(curve.Count);
        for (int i = 0; i < curve.Count; i++)
        {
            int applied = i < deltas.Length ? deltas[i] : 0;
            pts.Add(new VfPoint(curve[i].VoltageUv, curve[i].FrequencyKhz - applied));
        }
        return pts;
    }

    // ---- voltage lock ------------------------------------------------------
    //
    // The delta table has 80 slots but the curve has 103 points, and a uniform write to those 80
    // slots shifts all 103 — so the driver derives the top 23 from the table rather than exposing
    // them. That makes "flatten everything above 1000 mV" unreachable through the table: every point
    // it needs to move lives in the derived region.
    //
    // NvAPI_GPU_SetClockBoostLock says the same thing directly: pin the boost to a voltage. That is
    // the right primitive for a voltage cap, and it works regardless of where the curve point sits.

    /// <summary>How the cap is currently being held, so the UI and diagnostics can say which.</summary>
    public string VoltageLockMechanism { get; private set; } = "none";

    /// <summary>Clock cap applied through NVML, in MHz; 0 when none. Tracked because NVML has no getter.</summary>
    private int _nvmlClockCapMhz;
    private int _nvmlCapVoltageMv;

    /// <summary>The NVAPI write shape this driver honours, found by probing and then reused.</summary>
    private int _lockRecipe = -1;

    /// <summary>Set once NVML has held a cap: this driver's private lock is unverifiable, so stop asking.</summary>
    private bool _preferNvml;

    /// <summary>
    /// Cap the core at a voltage. 0 clears the cap.
    ///
    /// Two mechanisms, tried in that order:
    ///
    ///   1. NvAPI_GPU_SetClockBoostLock — the private "pin the boost to this voltage" call. It returns
    ///      Ok on this driver whether or not it does anything, so the write is proved by reading the
    ///      lock back; several struct shapes are tried and the one that sticks is remembered.
    ///   2. NVML locked clocks (what `nvidia-smi -lgc` does) — public, documented, and reliable. The
    ///      V/F curve says which frequency the target voltage reaches, and capping the clock there
    ///      stops the card asking for more volts. Same end result, different lever.
    /// </summary>
    public void SetVoltageLock(int gpuIndex, int targetMv)
    {
        var g = Gpu(gpuIndex);
        // Defensive: this is public API and the value ends up multiplied into microvolts, so a wild
        // figure would both be nonsense to the driver and risk overflowing the conversion.
        if (targetMv > 0)
        {
            var caps = GetCapabilities(gpuIndex);
            int lo = caps.MinVoltageMv > 0 ? caps.MinVoltageMv : 600;
            int hi = caps.MaxVoltageMv > 0 ? caps.MaxVoltageMv : 1200;
            targetMv = Math.Clamp(targetMv, lo, hi);
        }

        if (targetMv <= 0)
        {
            string? nvmlErr = ClearNvmlClockCap(gpuIndex);
            TryWriteBoostLock(g, 0, _lockRecipe >= 0 ? _lockRecipe : 0);
            VoltageLockMechanism = "none";
            if (nvmlErr != null && _nvmlClockCapMhz > 0)
                throw new GpuBackendException("Failed to release the clock cap: " + nvmlErr);
            return;
        }

        // ---- 1. private boost lock, proved by read-back
        if (!_preferNvml)
        {
            var recipes = _lockRecipe >= 0 ? new[] { _lockRecipe } : new[] { 0, 1, 2, 3 };
            foreach (int r in recipes)
            {
                try { TryWriteBoostLock(g, targetMv, r); } catch (GpuBackendException) { continue; }
                if (Math.Abs(ReadBoostLockMv(g) - targetMv) <= 10)
                {
                    _lockRecipe = r;
                    ClearNvmlClockCap(gpuIndex);          // one mechanism at a time
                    VoltageLockMechanism = $"NVAPI boost lock (recipe {r})";
                    return;
                }
            }
        }

        // ---- 2. NVML clock cap at the frequency the curve reaches at that voltage
        int mhz = CurveFrequencyAtMv(g, targetMv);
        if (mhz <= 0)
            throw new GpuBackendException(
                $"The driver ignored the {targetMv} mV voltage lock and the V/F curve could not be read, " +
                "so there is no frequency to cap instead.");

        mhz = Math.Max(210, mhz);
        int nvmlIndex = NvmlIndexFor(gpuIndex);
        string? err = Nvml.LockGraphicsClocks(nvmlIndex, 0, mhz);
        if (err != null)
        {
            // Some driver branches reject a 0 floor; 210 MHz is the lowest 3D clock these cards use.
            string? retry = Nvml.LockGraphicsClocks(nvmlIndex, 210, mhz);
            err = retry == null ? null : $"{err}; with a 210 MHz floor: {retry}";
        }
        if (err != null)
            throw new GpuBackendException(
                $"The driver ignored the {targetMv} mV voltage lock, and capping the clock at {mhz} MHz " +
                $"instead failed: {err}");

        _nvmlClockCapMhz = mhz;
        _nvmlCapVoltageMv = targetMv;
        _preferNvml = true;    // don't re-probe four unverifiable NVAPI writes on every apply
        // Only one lever should be holding the cap, and this is the one that just proved it works.
        try { TryWriteBoostLock(g, 0, 0); } catch (GpuBackendException) { }
        VoltageLockMechanism = $"NVML clock cap {mhz} MHz";
    }

    /// <summary>One attempt at the private lock. Recipes differ in the struct shape handed to the driver.</summary>
    private void TryWriteBoostLock(PhysicalGPU g, int targetMv, int recipe)
    {
        var mode = targetMv > 0 ? ClockLockMode.Manual : ClockLockMode.None;
        uint uv = targetMv > 0 ? (uint)(targetMv * 1000) : 0u;
        try
        {
            PrivateClockBoostLockV2 payload;
            switch (recipe)
            {
                case 0:   // just the graphics domain
                    payload = new PrivateClockBoostLockV2(new[]
                    {
                        new PrivateClockBoostLockV2.ClockBoostLock(PublicClockDomain.Graphics, mode, uv)
                    });
                    break;

                case 1:   // read-modify-write: keep every entry the driver reported, change graphics only
                case 3:
                {
                    var current = GPUApi.GetClockBoostLock(g.Handle).ClockBoostLocks;
                    var entries = current.Length > 0
                        ? current.Select(e => e.ClockDomain == PublicClockDomain.Graphics
                            ? new PrivateClockBoostLockV2.ClockBoostLock(PublicClockDomain.Graphics, mode, uv)
                            : new PrivateClockBoostLockV2.ClockBoostLock(e.ClockDomain, ClockLockMode.None, 0)).ToArray()
                        : new[] { new PrivateClockBoostLockV2.ClockBoostLock(PublicClockDomain.Graphics, mode, uv) };
                    if (recipe == 3) EnableOcPStates(g);
                    payload = new PrivateClockBoostLockV2(entries);
                    break;
                }

                default:  // 2: unlock the overclocked p-states first, then the single-entry write
                    EnableOcPStates(g);
                    payload = new PrivateClockBoostLockV2(new[]
                    {
                        new PrivateClockBoostLockV2.ClockBoostLock(PublicClockDomain.Graphics, mode, uv)
                    });
                    break;
            }

            GPUApi.SetClockBoostLock(g.Handle, payload);
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to set the voltage lock to {targetMv} mV: {ex.Status}.", ex);
        }
        catch (NVIDIANotSupportedException ex)
        {
            throw new GpuBackendException("This driver does not expose the voltage lock.", ex);
        }
    }

    private void EnableOcPStates(PhysicalGPU g)
    {
        if (_ocEnabled) return;
        _ocEnabled = true;
        try { GPUApi.EnableOverclockedPStates(g.Handle); }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
    }

    /// <summary>
    /// NVML enumerates by PCI bus and NVAPI does not, so on a multi-GPU box the ordinals disagree.
    /// Match on the adapter name and only fall back to the ordinal when that finds nothing.
    /// </summary>
    private int NvmlIndexFor(int gpuIndex)
    {
        if (_nvmlIndex.TryGetValue(gpuIndex, out int cached)) return cached;
        int resolved = gpuIndex;
        string want = _devices[gpuIndex].Name;
        for (int i = 0; i < 8; i++)
        {
            string? name = Nvml.DeviceName(i);
            if (name == null) break;
            if (string.Equals(name.Trim(), want.Trim(), StringComparison.OrdinalIgnoreCase)) { resolved = i; break; }
        }
        _nvmlIndex[gpuIndex] = resolved;
        return resolved;
    }
    private readonly Dictionary<int, int> _nvmlIndex = new();

    private string? ClearNvmlClockCap(int gpuIndex)
    {
        if (_nvmlClockCapMhz <= 0) return null;
        string? err = Nvml.ResetGraphicsClocks(NvmlIndexFor(gpuIndex));
        if (err == null) { _nvmlClockCapMhz = 0; _nvmlCapVoltageMv = 0; }
        return err;
    }

    /// <summary>Highest stock curve frequency at or below <paramref name="mv"/>, in MHz. 0 if unknown.</summary>
    private int CurveFrequencyAtMv(PhysicalGPU g, int mv)
    {
        try
        {
            int best = 0;
            foreach (var p in ReadBaseCurve(g))
                if (p.VoltageUv > 0 && p.FrequencyKhz > 0 && p.VoltageUv <= (uint)mv * 1000)
                    best = Math.Max(best, p.FrequencyKhz / 1000);
            return best;
        }
        catch (NVIDIAApiException) { return 0; }
        catch (NVIDIANotSupportedException) { return 0; }
    }

    /// <summary>The voltage the core is currently capped to, or 0 when uncapped.</summary>
    public int ReadVoltageLockMv(int gpuIndex)
    {
        int mv = ReadBoostLockMv(Gpu(gpuIndex));
        if (mv > 0) return mv;
        // NVML exposes no getter for the locked clock range, so a cap we applied is tracked in-process.
        return _nvmlClockCapMhz > 0 ? _nvmlCapVoltageMv : 0;
    }

    private static int ReadBoostLockMv(PhysicalGPU g)
    {
        try
        {
            foreach (var e in GPUApi.GetClockBoostLock(g.Handle).ClockBoostLocks)
                if (e.LockMode == ClockLockMode.Manual && e.VoltageInMicroV > 0)
                    return (int)(e.VoltageInMicroV / 1000);
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
        return 0;
    }

    /// <summary>The write recipe this driver actually honours, found by probing and then reused.</summary>
    private (uint[] mask, int entryFlag)? _writeRecipe;

    /// <summary>
    /// Write per-point deltas and prove they landed.
    ///
    /// SetClockBoostTable returns Ok on this driver whether or not it does anything, so success from
    /// the call means nothing. Three things are unknown: which mask the write wants, which of the two
    /// trailing arrays holds the high points, and whether each changed entry needs a validity flag.
    /// Rather than guess, snapshot the curve, try each combination, and keep the one that moves it.
    /// </summary>
    private void WriteCurveDeltas(PhysicalGPU g, int[] deltas)
    {
        var (entries, ver) = CurveLayout(g);

        // Some drivers refuse curve edits until overclocked p-states are unlocked. Harmless if absent.
        if (!_ocEnabled)
        {
            _ocEnabled = true;
            try { GPUApi.EnableOverclockedPStates(g.Handle); }
            catch (NVIDIAApiException) { }
            catch (NVIDIANotSupportedException) { }
        }

        var targeted = Enumerable.Range(0, deltas.Length).Where(i => deltas[i] != 0).ToArray();

        // Clearing the table can't be verified by "did anything move" — just use what we know works.
        if (targeted.Length == 0)
        {
            var (m0, f0) = _writeRecipe ?? (CurveMask(g), -1);
            if (NvApiPrivate.WriteDeltasRaw(g.Handle, m0, entries, ver, deltas, f0) == 0) return;
            NvApiPrivate.WriteDeltas(g.Handle, CurveMask(g), deltas);
            return;
        }

        var before = ReadEffectiveCurve(g);
        bool Moved()
        {
            var after = ReadEffectiveCurve(g);
            foreach (int i in targeted)
                if (i < after.Count && i < before.Count && after[i].FrequencyKhz != before[i].FrequencyKhz)
                    return true;
            return false;
        }

        // Known-good recipe first, then the grid. Two dimensions used to be probed here and are
        // gone: which trailing array held the high points, and whether to drop them entirely. Both
        // existed because the delta table was modelled as 80 entries followed by two arrays of bare
        // ints. It is one flat run of 36-byte entries, so those points were being written into the
        // middle of entry 80's record — the driver validates that record and refused the whole call.
        // With the offsets right there is nothing left for either dimension to select, and dropping
        // them takes the worst case from 32 write-and-verify cycles to 8.
        var recipes = new List<(uint[] mask, int entryFlag)>();
        if (_writeRecipe != null) recipes.Add(_writeRecipe.Value);
        foreach (var mask in new[] { CurveMask(g), NvApiPrivate.Mask103, NvApiPrivate.MaskFor(deltas), NvApiPrivate.FullMask })
            foreach (int flag in new[] { -1, 1 })
                recipes.Add((mask, flag));

        foreach (var r in recipes)
        {
            if (NvApiPrivate.WriteDeltasRaw(g.Handle, r.mask, entries, ver, deltas, r.entryFlag) != 0) continue;
            if (!Moved()) continue;
            _writeRecipe = r;
            return;
        }

        // Last resort: NvAPIWrapper's own typed call.
        if (NvApiPrivate.WriteDeltas(g.Handle, CurveMask(g), deltas) == 0 && Moved()) return;

        throw new GpuBackendException(
            "The driver accepted the V/F curve write but the curve did not change. " +
            "Close MSI Afterburner or any other tuning tool holding the curve, then try again.");
    }

    private bool _ocEnabled;

    private (int min, int max) BoostDeltaRange(PhysicalGPU g)
    {
        try
        {
            foreach (var r in GPUApi.GetClockBoostRanges(g.Handle).ClockBoostRanges)
                if (r.MinimumInkHz != 0 || r.MaximumInkHz != 0)
                    // Never hand a reversed pair to Math.Clamp — it throws ArgumentException.
                    return r.MinimumInkHz <= r.MaximumInkHz
                        ? (r.MinimumInkHz, r.MaximumInkHz)
                        : (r.MaximumInkHz, r.MinimumInkHz);
        }
        catch (NVIDIAApiException) { }
        catch (NVIDIANotSupportedException) { }
        return (-1_000_000, 1_000_000);   // ±1 GHz, generous fallback
    }

    /// <summary>
    /// Apply a negative voltage offset by flattening the V/F curve that far below its stock top.
    /// offsetMv == 0 clears the curve edit entirely.
    /// </summary>
    public void SetVoltageCurveOffset(int gpuIndex, int offsetMv, int extraClockMhz = 0)
    {
        var g = Gpu(gpuIndex);
        try
        {
            if (offsetMv >= 0)
            {
                // Clear the curve deltas ONLY. This used to drop the voltage lock as well, which was
                // fine while a negative offset was how undervolts were expressed — but once one
                // absolute slider drove both levers, every apply passed 0 here and silently undid the
                // cap that had been written moments earlier. The lock belongs to SetVoltageLock.
                WriteCurveDeltas(g, new int[NvApiPrivate.TotalPoints(CurveLayout(g).entries)]);
                return;
            }

            var basePts = ReadBaseCurve(g);
            uint stockMaxUv = VfCurve.MaxVoltageUv(basePts);
            if (stockMaxUv == 0) throw new GpuBackendException("Could not read the V/F curve (no valid points).");

            long capUv = (long)stockMaxUv + offsetMv * 1000L;
            if (capUv < 500_000) capUv = 500_000;   // never cap below 500 mV — that would be nonsense

            var (rMin, rMax) = BoostDeltaRange(g);
            // The lock is what actually enforces the cap; the flatten only reaches points the table
            // owns (below ~945 mV here) and is applied as a belt-and-braces extra.
            SetVoltageLock(gpuIndex, (int)(capUv / 1000));
            try { WriteCurveDeltas(g, VfCurve.ComputeFlattenDeltas(basePts, (uint)capUv, extraClockMhz * 1000, rMin, rMax)); }
            catch (GpuBackendException) { /* lock already carries the cap */ }
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException(
                $"Failed to apply {offsetMv} mV voltage offset: {ex.Status}. " +
                "Curve editing needs admin and no other OC tool holding the curve.", ex);
        }
        catch (NVIDIANotSupportedException ex)
        {
            // Not an NVIDIAApiException — thrown when nvapi64.dll has no such entry point (pre-Pascal
            // card or a driver branch that dropped the private curve calls).
            throw new GpuBackendException(
                $"Failed to apply {offsetMv} mV voltage offset: this driver/GPU does not expose the " +
                "private V/F curve calls.", ex);
        }
    }

    public IReadOnlyList<VfCurveSample> ReadVfCurve(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        try
        {
            var basePts = ReadBaseCurve(g);
            var eff = ReadEffectiveCurve(g);
            var list = new List<VfCurveSample>();
            for (int i = 0; i < basePts.Count; i++)
            {
                if (basePts[i].VoltageUv == 0 || basePts[i].FrequencyKhz == 0) continue;
                int live = i < eff.Count ? eff[i].FrequencyKhz : basePts[i].FrequencyKhz;
                list.Add(new VfCurveSample(i, (int)(basePts[i].VoltageUv / 1000), basePts[i].FrequencyKhz / 1000, live / 1000));
            }
            return list;
        }
        catch (NVIDIAApiException) { return Array.Empty<VfCurveSample>(); }
        catch (NVIDIANotSupportedException) { return Array.Empty<VfCurveSample>(); }
    }

    public void SetVfCurveTargets(int gpuIndex, IReadOnlyList<VfCurveSample> targets)
    {
        var g = Gpu(gpuIndex);
        try
        {
            if (targets.Count == 0) { WriteCurveDeltas(g, new int[NvApiPrivate.TotalPoints(CurveLayout(g).entries)]); return; }

            var basePts = ReadBaseCurve(g);
            var (rMin, rMax) = BoostDeltaRange(g);
            var targetKhz = targets
                .Where(t => t.Index >= 0 && t.Index < basePts.Count)
                .GroupBy(t => t.Index)
                .ToDictionary(grp => grp.Key, grp => grp.Last().LiveMhz * 1000);

            WriteCurveDeltas(g, VfCurve.ComputeTargetDeltas(basePts, targetKhz, rMin, rMax));
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to write V/F curve: {ex.Status}.", ex);
        }
        catch (NVIDIANotSupportedException ex)
        {
            throw new GpuBackendException("V/F curve editing is not supported by this driver.", ex);
        }
    }

    public void SetVoltageBoost(int gpuIndex, int percent)
    {
        var g = Gpu(gpuIndex);
        try
        {
            GPUApi.SetCoreVoltageBoostPercent(g.Handle, new PrivateVoltageBoostPercentV1((uint)Math.Clamp(percent, 0, 100)));
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to set voltage boost to {percent}%: {ex.Status}.", ex);
        }
    }

    public void SetTempLimit(int gpuIndex, int celsius)
    {
        var g = Gpu(gpuIndex);
        try
        {
            var current = GPUApi.GetThermalPoliciesStatus(g.Handle);
            var entries = current.ThermalPoliciesStatusEntries;
            if (entries.Length == 0) throw new GpuBackendException("GPU reports no thermal policy entries.");

            var first = entries[0];
            var updated = new PrivateThermalPoliciesStatusV2(new[]
            {
                new PrivateThermalPoliciesStatusV2.ThermalPoliciesStatusEntry(first.PerformanceStateId, first.Controller, celsius)
            });
            GPUApi.SetThermalPoliciesStatus(g.Handle, updated);
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to set temperature limit to {celsius} °C: {ex.Status}.", ex);
        }
    }

    public void SetFanSpeed(int gpuIndex, int fanIndex, int percent)
    {
        var g = Gpu(gpuIndex);
        try
        {
            var coolers = g.CoolerInformation.Coolers.ToList();
            if (coolers.Count == 0) throw new GpuBackendException("No controllable fans reported by NVAPI.");

            foreach (var c in coolers)
            {
                if (fanIndex >= 0 && c.CoolerId != fanIndex) continue;
                int lvl = Math.Clamp(percent, c.DefaultMinimumLevel, c.DefaultMaximumLevel <= 0 ? 100 : c.DefaultMaximumLevel);
                g.CoolerInformation.SetCoolerSettings(c.CoolerId, CoolerPolicy.Manual, lvl);
            }
        }
        catch (NVIDIAApiException ex)
        {
            throw new GpuBackendException($"Failed to set fan speed to {percent}%: {ex.Status}.", ex);
        }
    }

    public void SetFanAuto(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        try
        {
            g.CoolerInformation.RestoreCoolerSettingsToDefault();
        }
        catch (NVIDIAApiException)
        {
            // Some RTX cards reject the legacy restore; fall back to per-cooler Auto policy.
            try
            {
                foreach (var c in g.CoolerInformation.Coolers.ToList())
                    g.CoolerInformation.SetCoolerSettings(c.CoolerId, CoolerPolicy.None);
            }
            catch (NVIDIAApiException ex)
            {
                throw new GpuBackendException($"Failed to return fans to automatic control: {ex.Status}.", ex);
            }
        }
    }

    public void ResetToDefaults(int gpuIndex)
    {
        // The rail ceiling survives a driver-level reset, so clear it explicitly or a raised
        // ceiling would outlive the Reset button that appears to undo everything.
        // The crossbar offset has a real default of zero. The voltage rails do not: the driver
        // reports no factory value, and this layer cannot tell an untouched rail from one a previous
        // run raised. TuningService restores those from the figure recorded on first sight.
        try { SetXbarOffset(gpuIndex, 0); } catch (GpuBackendException) { }

        var caps = GetCapabilities(gpuIndex);
        var errors = new List<string>();
        void Try(Action a) { try { a(); } catch (Exception e) { errors.Add(e.Message); } }

        if (caps.CanSetCoreOffset) Try(() => SetCoreOffset(gpuIndex, 0));
        if (caps.CanSetMemoryOffset) Try(() => SetMemoryOffset(gpuIndex, 0));
        if (caps.CanSetPowerLimit) Try(() => SetPowerLimit(gpuIndex, caps.PowerLimitDefaultPercent));
        if (caps.CanSetTempLimit) Try(() => SetTempLimit(gpuIndex, caps.TempLimitDefaultC));
        if (caps.CanSetVoltageBoost) Try(() => SetVoltageBoost(gpuIndex, 0));
        if (caps.CanSetVoltageCurve) Try(() => SetVoltageCurveOffset(gpuIndex, 0));   // clears the whole delta table
        // Both cap mechanisms unconditionally: an NVML clock lock outlives the process that set it,
        // so a fresh launch has to be able to clear one it doesn't remember applying.
        Try(() => TryWriteBoostLock(Gpu(gpuIndex), 0, 0));
        Try(() =>
        {
            string? e = Nvml.ResetGraphicsClocks(NvmlIndexFor(gpuIndex));
            _nvmlClockCapMhz = 0; _nvmlCapVoltageMv = 0; VoltageLockMechanism = "none";
            // NVML is optional: only complain if it is actually there and still refused.
            if (e != null && Nvml.IsAvailable) throw new GpuBackendException("Clock cap: " + e);
        });
        if (caps.CanSetFanSpeed) Try(() => SetFanAuto(gpuIndex));

        if (errors.Count > 0) throw new GpuBackendException("Reset partially failed: " + string.Join(" | ", errors));
    }

    public string GetDiagnostics(int gpuIndex)
    {
        var g = Gpu(gpuIndex);
        var sb = new System.Text.StringBuilder();
        void Section(string title, Action body)
        {
            sb.AppendLine($"--- {title} ---");
            try { body(); } catch (Exception e) { sb.AppendLine($"  <{e.GetType().Name}: {e.Message}>"); }
            sb.AppendLine();
        }

        sb.AppendLine($"GPU  : {_devices[gpuIndex].Name}");
        sb.AppendLine($"Driver {_devices[gpuIndex].DriverVersion}   vBIOS {_devices[gpuIndex].BiosVersion}");
        sb.AppendLine();

        Section("Power policy INFO (per entry)", () =>
        {
            foreach (var i in g.PerformanceControl.PowerLimitInformation)
                sb.AppendLine($"  pstate={i.PerformanceStateId}  minPCM={i.MinimumPowerInPCM}  defPCM={i.DefaultPowerInPCM}  maxPCM={i.MaximumPowerInPCM}");
        });

        Section("Power policy STATUS (raw entries)", () =>
        {
            var st = GPUApi.ClientPowerPoliciesGetStatus(g.Handle);
            var entries = st.PowerPolicyStatusEntries;
            sb.AppendLine($"  count={entries.Length}");
            for (int i = 0; i < entries.Length; i++)
                sb.AppendLine($"  [{i}] pstate={entries[i]._PerformanceStateId} unk1={entries[i]._Unknown1} " +
                              $"powerPCM={entries[i]._PowerTargetInPCM} unk2={entries[i]._Unknown2}");
        });

        Section("Thermal policy", () =>
        {
            foreach (var i in g.PerformanceControl.ThermalLimitInformation)
                sb.AppendLine($"  info  ctrl={i.Controller} min={i.MinimumTemperature} def={i.DefaultTemperature} max={i.MaximumTemperature}");
            foreach (var p in g.PerformanceControl.ThermalLimitPolicies)
                sb.AppendLine($"  status ctrl={p.Controller} pstate={p.PerformanceStateId} target={p.TargetTemperature}");
        });

        Section("Voltage", () =>
        {
            sb.AppendLine($"  current      = {GPUApi.GetCurrentVoltage(g.Handle).ValueInMicroVolt / 1000.0:0} mV");
            sb.AppendLine($"  boost percent= {GPUApi.GetCoreVoltageBoostPercent(g.Handle).Percent}");
            sb.AppendLine($"  lock (cap)   = {ReadVoltageLockMv(gpuIndex)} mV   (0 = none)");
            sb.AppendLine($"  mechanism    = {VoltageLockMechanism}");
            foreach (var e in GPUApi.GetClockBoostLock(g.Handle).ClockBoostLocks)
                sb.AppendLine($"  lock entry   : domain={e.ClockDomain} mode={e.LockMode} volt={e.VoltageInMicroV / 1000.0:0} mV");
            sb.AppendLine($"  NVML         = {(Nvml.IsAvailable ? "available" : "unavailable")}" +
                          (Nvml.LastError is { } le ? $" ({le})" : "") +
                          (Nvml.DeviceName(NvmlIndexFor(gpuIndex)) is { } nm ? $", device[{NvmlIndexFor(gpuIndex)}] = {nm}" : ""));
            foreach (int mv in new[] { 900, 950, 1000, 1050 })
                sb.AppendLine($"  curve says {mv} mV -> {CurveFrequencyAtMv(g, mv)} MHz (clock cap used if the lock is ignored)");
        });

        Section("PStates20 (P0 clock deltas)", () =>
        {
            var ps = GPUApi.GetPerformanceStates20(g.Handle);
            sb.AppendLine($"  editable={ps.IsEditable}");
            if (ps.Clocks.TryGetValue(PerformanceStateId.P0_3DPerformance, out var clocks))
                foreach (var c in clocks)
                    sb.AppendLine($"  {c.DomainId,-9} editable={c.IsEditable} delta={c.FrequencyDeltaInkHz.DeltaValue / 1000} MHz " +
                                  $"range=[{c.FrequencyDeltaInkHz.DeltaRange.Minimum / 1000}..{c.FrequencyDeltaInkHz.DeltaRange.Maximum / 1000}]");
        });

        // Every clock domain the driver will admit to, including the ones NvAPIWrapper drops on the
        // floor. NV_GPU_CLOCK_FREQUENCIES carries 32 domain slots and the wrapper only names four
        // (0 Graphics, 4 Memory, 7 Processor, 8 Video), so an undocumented domain — XBAR is the one
        // people ask about on Blackwell — would sit in a slot nobody normally looks at.
        //
        // Read-only. On a 4070 Ti this reports slots 0 and 4 and nothing else, which is how we know
        // Ada exposes no XBAR through NVAPI; run it on a 50-series card to see whether that changes.
        Section("Crossbar (XBAR) probe", () =>
        {
            var x = NvApiPrivate.ProbeXbar(g.Handle);
            sb.AppendLine($"  entry points resolved: info={x.InfoResolved} control={x.ControlResolved} " +
                          $"set={x.SetResolved} measure={x.MeasureResolved}");
            static string St(int s) => s == int.MinValue ? "not called / threw" : s == 0 ? "0 (Ok)" : s.ToString();
            sb.AppendLine($"  GetInfo status    : {St(x.InfoStatus)}");
            sb.AppendLine($"  GetControl status : {St(x.ControlStatus)}");
            sb.AppendLine($"  measured clocks   : core {x.CoreKhz / 1000.0:N0} MHz, " +
                          $"xbar {x.XbarKhz / 1000.0:N0} MHz, memory {x.MemoryKhz / 1000.0:N0} MHz");
            if (x.CoreKhz > 0 && x.XbarKhz > 0)
                sb.AppendLine($"  xbar : core ratio = {(double)x.XbarKhz / x.CoreKhz:F4}");
            sb.AppendLine(x.EntryTypes.Length == 0
                ? "  info entry types  : <none read>"
                : "  info entry types  : " + string.Join(",", x.EntryTypes));
            sb.AppendLine($"  type-1 entry index: {x.TypeOneIndex}");

            // Every domain the family lists, not just the crossbar. The control offset is predicted
            // from the block arithmetic the crossbar confirmed; a capture taken while another tool
            // holds a domain at a known offset is what turns a prediction into a fact.
            sb.AppendLine("  domains listed (slot / type / range / predicted control offset):");
            foreach (var de in NvApiPrivate.ReadDomainEntries(g.Handle))
                sb.AppendLine($"    slot {de.Index,2}  type {de.Type,3}  " +
                              (de.HasRange ? $"{de.MinMhz,6}..{de.MaxMhz,-6} MHz" : "no range found    ") +
                              $"  -> +0x{NvApiPrivate.ControlOffsetFor(de.Index):X4}" +
                              // The measure family numbers domains the same way the info family types
                              // them - 0 core, 1 crossbar, 4 memory all agree - so a domain that
                              // reports a plausible frequency here is the one this slot describes.
                              $"   measured {NvApiPrivate.MeasureClockKhz(g.Handle, de.Type) / 1000,6} MHz");
            if (x.TypeOneWords.Length > 0)
            {
                sb.AppendLine("  type-1 entry words (offset: value  [signed lo/hi halves]):");
                for (int i = 0; i < x.TypeOneWords.Length; i++)
                {
                    int w = x.TypeOneWords[i];
                    short lo = unchecked((short)(w & 0xFFFF)), hi = unchecked((short)(w >> 16));
                    sb.AppendLine($"      +0x{i * 4:X3}  {w,12}  0x{w:X8}  [{lo,6} / {hi,6}]");
                }
            }
            if (x.ControlWords.Length > 0)
            {
                sb.AppendLine($"  control buffer, non-zero words ({x.ControlWords.Length / 2} shown):");
                for (int i = 0; i + 1 < x.ControlWords.Length; i += 2)
                    sb.AppendLine($"      +0x{x.ControlWords[i]:X4}  {x.ControlWords[i + 1],12}  0x{x.ControlWords[i + 1]:X8}");
            }
            else sb.AppendLine("  control buffer: entirely zero");
            sb.AppendLine("  GetControl shapes (read-only, how much the driver fills in):");
            foreach (var (label, status, nonZero, head) in NvApiPrivate.ExploreXbarControl(g.Handle))
                sb.AppendLine($"      {label,-32} status={(status == int.MinValue ? "threw" : status.ToString()),-5} " +
                              $"nonZero={nonZero,-4} {head}");
            sb.AppendLine("  GetControl accepted shapes (-9 = wrong struct version, 0 = accepted):");
            foreach (var (shape, status) in NvApiPrivate.ProbeXbarControlShapes(g.Handle))
                sb.AppendLine($"      {shape,-16} status={(status == int.MinValue ? "threw" : status.ToString())}");
            var xi = NvApiPrivate.ReadXbarInfo(g.Handle);
            sb.AppendLine($"  ReadXbarInfo      : supported={xi.Supported} range=[{xi.MinOffsetMhz}..{xi.MaxOffsetMhz}]");
            sb.AppendLine($"  current offset    : {NvApiPrivate.ReadXbarOffsetMhz(g.Handle)} MHz");
        });

        Section("Clock domains (all 32 slots, not just the named ones)", () =>
        {
            // Resolve on the runtime type: the property is typed IClockFrequencies and the wrapper
            // hands back V2 (or V3 on a newer driver), each declaring its own _Clocks array.
            void Dump(string label, object freqs)
            {
                var field = freqs.GetType().GetField("_Clocks",
                    System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance);
                if (field?.GetValue(freqs) is not ClockDomainInfo[] slots)
                {
                    sb.AppendLine($"  {label,-8} <could not read the raw slot array>");
                    return;
                }
                var live = Enumerable.Range(0, slots.Length)
                                     .Where(i => slots[i].IsPresent || slots[i].Frequency != 0).ToArray();
                sb.AppendLine($"  {label,-8} {live.Length} of {slots.Length} slots populated: [{string.Join(", ", live)}]");
                foreach (int i in live)
                {
                    string name = i switch
                    {
                        0 => "Graphics", 4 => "Memory", 7 => "Processor", 8 => "Video",
                        _ => "UNDOCUMENTED  <-- not a public domain"
                    };
                    sb.AppendLine($"      slot {i,2}  {slots[i].Frequency / 1000.0,9:N1} MHz  {name}");
                }
            }

            Dump("current", g.CurrentClockFrequencies);
            Dump("base", g.BaseClockFrequencies);
            Dump("boost", g.BoostClockFrequencies);

            // The other surface that enumerates domains: a tunable domain the wrapper has no name for
            // shows up here as a raw id rather than an enum member.
            var ps = GPUApi.GetPerformanceStates20(g.Handle);
            sb.AppendLine($"  pstate domains (editable={ps.IsEditable}):");
            foreach (var kv in ps.Clocks)
                foreach (var c in kv.Value)
                {
                    bool named = Enum.IsDefined(typeof(PublicClockDomain), c.DomainId);
                    sb.AppendLine($"      {kv.Key,-20} domainId={(int)c.DomainId,-3} " +
                                  $"{(named ? c.DomainId.ToString() : "UNDOCUMENTED"),-14} editable={c.IsEditable}");
                }
        });

        Section("V/F curve — private call probe", () =>
        {
            var (cb, tb) = NvApiPrivate.StructSizes();
            sb.AppendLine($"  entry points resolved = {NvApiPrivate.CurveAvailable}");
            sb.AppendLine($"  struct sizes: curve={cb} table={tb}");
            uint[] boostMask = Array.Empty<uint>();
            try { boostMask = GPUApi.GetClockBoostMask(g.Handle)._Masks ?? Array.Empty<uint>(); }
            catch (Exception e) { sb.AppendLine($"  GetClockBoostMask failed: {e.GetType().Name}"); }
            if (boostMask.Length == 4)
                sb.AppendLine($"  boost mask = {boostMask[0]:X8} {boostMask[1]:X8} {boostMask[2]:X8} {boostMask[3]:X8}");

            foreach (var (label, mask) in new (string, uint[])[]
                     { ("driver mask", boostMask.Length == 4 ? boostMask : NvApiPrivate.FullMask),
                       ("all-ones",    NvApiPrivate.FullMask) })
            {
                var pts = NvApiPrivate.ReadCurve(g.Handle, mask);
                int valid = pts.Count(x => x.voltUv > 0 && x.freqKhz > 0);
                sb.AppendLine($"  ReadCurve[{label}] -> {valid} valid point(s)" +
                              (valid > 0 ? $", {pts.Where(x => x.voltUv > 0).Min(x => x.voltUv) / 1000}–{pts.Max(x => x.voltUv) / 1000} mV" : ""));
                var dl = NvApiPrivate.ReadDeltas(g.Handle, mask);
                sb.AppendLine($"  ReadDeltas[{label}] -> {dl.Length} slot(s), {dl.Count(x => x != 0)} non-zero");
            }
            sb.AppendLine($"  chosen mask = 0x{CurveMask(g)[0]:X8}");
            sb.AppendLine("  layout probe (entries x version -> status / points / mV range / max MHz):");
            foreach (var pr in NvApiPrivate.ProbeCurveLayouts(g.Handle))
                sb.AppendLine($"    {pr.GpuEntries,4} x v{pr.Version}  status={pr.Status,-3} " +
                              (pr.Status == 0 ? $"points={pr.ValidPoints,4}  {pr.MinMv}-{pr.MaxMv} mV  max {pr.MaxMhz} MHz" : ""));
            var chosen = CurveLayout(g);
            // The points actually read, not what the struct's field split would allow: those two
            // disagree by 24 on a 5070 Ti, and a bug report saying "103 curve points" next to a probe
            // line saying 127 sends the reader after the wrong thing.
            int chosenPoints = ReadEffectiveCurve(g).Count;
            sb.AppendLine($"  chosen layout = {chosen.entries} entries, version {chosen.version}, " +
                          $"{chosenPoints} curve points (buffer holds up to {NvApiPrivate.TotalPoints(chosen.entries)})");
            var (arrA, arrB) = NvApiPrivate.ReadTrailingArrays(g.Handle, CurveMask(g), chosen.entries, chosen.version);
            if (arrA.Length > 0)
            {
                sb.AppendLine($"  trailing array 0 = {string.Join(",", arrA)}");
                sb.AppendLine($"  trailing array 1 = {string.Join(",", arrB)}");
                sb.AppendLine(_writeRecipe == null
                    ? "  write recipe: not yet established (no curve write attempted this session)"
                    : $"  write recipe: mask {_writeRecipe.Value.mask[0]:X8}-{_writeRecipe.Value.mask[3]:X8}, " +
                      $"entry flag {_writeRecipe.Value.entryFlag}");
            }
        });

        Section("V/F curve (stock, valid points only)", () =>
        {
            var basePts = ReadBaseCurve(g);
            var eff = ReadEffectiveCurve(g);
            sb.AppendLine($"  stock max voltage = {VfCurve.MaxVoltageUv(basePts) / 1000.0:0} mV");
            var (rMin, rMax) = BoostDeltaRange(g);
            sb.AppendLine($"  delta range = [{rMin / 1000}..{rMax / 1000}] MHz");
            uint cap = VfCurve.InferCapVoltageUv(eff);
            sb.AppendLine($"  inferred cap = {(cap == 0 ? "none (curve not flattened)" : (cap / 1000.0).ToString("0") + " mV")}");
            for (int i = 0; i < basePts.Count; i++)
            {
                if (basePts[i].VoltageUv == 0 || basePts[i].FrequencyKhz == 0) continue;
                sb.AppendLine($"  [{i,2}] {basePts[i].VoltageUv / 1000.0,6:0} mV  stock {basePts[i].FrequencyKhz / 1000,5} MHz  " +
                              $"live {eff[i].FrequencyKhz / 1000,5} MHz");
            }
        });

        Section("Voltage lock (SetClockBoostLock)", () =>
        {
            var l = GPUApi.GetClockBoostLock(g.Handle);
            foreach (var e in l.ClockBoostLocks)
                sb.AppendLine($"  domain={e.ClockDomain} mode={e.LockMode} voltage={e.VoltageInMicroV / 1000.0:0} mV");
            sb.AppendLine($"  -> interpreted lock = {ReadVoltageLockMv(gpuIndex)} mV (0 = unlocked)");
        });

        Section("Coolers", () =>
        {
            foreach (var c in g.CoolerInformation.Coolers)
                sb.AppendLine($"  id={c.CoolerId} policy={c.CurrentPolicy} level={c.CurrentLevel}% rpm={c.CurrentFanSpeedInRPM} " +
                              $"range=[{c.DefaultMinimumLevel}..{c.DefaultMaximumLevel}]");
        });

        Section("Thermal settings (public API sensors)", () =>
        {
            foreach (var s in g.ThermalInformation.ThermalSensors)
                sb.AppendLine($"  target={s.Target} ctrl={s.Controller} temp={s.CurrentTemperature}");
        });

        Section("Private thermal sensors (all 32 slots)", () =>
        {
            var mask = NvApiPrivate.ProbeMask(g.Handle);
            sb.AppendLine($"  probed mask = 0x{mask:X}");
            if (mask == 0) { sb.AppendLine("  (call unavailable on this driver)"); return; }
            var slots = NvApiPrivate.Read(g.Handle, mask);
            for (int i = 0; i < slots.Length; i++)
                if (slots[i] != 0) sb.AppendLine($"  slot[{i,2}] = {slots[i]:0.0} °C");
            var (hs, mj) = NvApiPrivate.Interpret(_devices[gpuIndex].Name, slots);
            sb.AppendLine($"  -> interpreted hotspot={hs:0.0}  memory={mj:0.0}");
        });

        return sb.ToString();
    }

    public void Dispose()
    {
        if (_initialized)
        {
            try { NVIDIA.Unload(); } catch { /* ignore */ }
            _initialized = false;
        }
        // Only unload NVML if we ever loaded it. Note this does NOT release a clock cap — that is
        // deliberate, so a cap survives closing the app, exactly like nvidia-smi's own locks.
        Nvml.TryShutdown();
    }

    private static T SafeGet<T>(Func<T> f, T fallback)
    {
        try { return f(); } catch { return fallback; }
    }
}
