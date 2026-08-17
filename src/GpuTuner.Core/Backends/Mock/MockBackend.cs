using GpuTuner.Core.Models;

namespace GpuTuner.Core.Backends.Mock;

/// <summary>
/// Simulated GPU for developing / demoing the UI without hardware (run with --mock).
/// Produces plausible telemetry that reacts to the applied settings.
/// </summary>
public sealed class MockBackend : IGpuBackend
{
    private readonly Random _rng = new(42);
    private readonly List<GpuDevice> _devices = new()
    {
        new GpuDevice(0, "Simulated RTX 4070 SUPER", "Mock", "PCI bus 1, slot 0", "560.94", 12282, "95.04.4E.00.6B")
    };

    private int _core, _mem, _power = 100, _temp = 83, _fan = 30, _voltBoost, _voltOffset;
    private bool _fanManual;
    private double _load = 5, _simTemp = 40;

    public string BackendName => "Simulated GPU";
    public IReadOnlyList<GpuDevice> Devices => _devices;
    public void Initialize() { }

    public GpuCapabilities GetCapabilities(int gpuIndex) => new()
    {
        CanSetCoreOffset = true, CanSetMemoryOffset = true, CanSetPowerLimit = true,
        CanSetTempLimit = true, CanSetFanSpeed = true, CanReadVoltage = true, CanSetVoltageBoost = true, CanSetVoltageCurve = true,
        VoltageOffsetMinMv = -300, VoltageOffsetMaxMv = 0, StockMaxVoltageMv = 1100,
        MinVoltageMv = 850, MaxVoltageMv = 1160,
        CoreOffsetMinMhz = -1000, CoreOffsetMaxMhz = 1000,
        MemoryOffsetMinMhz = -2000, MemoryOffsetMaxMhz = 4000,
        PowerLimitMinPercent = 50, PowerLimitMaxPercent = 115, PowerLimitDefaultPercent = 100,
        TempLimitMinC = 65, TempLimitMaxC = 88, TempLimitDefaultC = 83,
        VoltageBoostMinPercent = 0, VoltageBoostMaxPercent = 100,
        FanMinPercent = 0, FanMaxPercent = 100, FanCount = 2
    };

    public GpuTelemetry ReadTelemetry(int gpuIndex)
    {
        // Random-walk load, temperature follows load minus fan cooling.
        _load = Math.Clamp(_load + (_rng.NextDouble() - 0.5) * 20, 0, 100);
        double fan = _fanManual ? _fan : Math.Clamp(30 + (_simTemp - 40) * 1.5, 30, 100);
        double targetTemp = 35 + _load * 0.45 * (_power / 100.0) - fan * 0.08;
        _simTemp += (targetTemp - _simTemp) * 0.15;
        _simTemp = Math.Min(_simTemp, _temp + 1);

        double coreClock = _load < 5 ? 210 : Math.Min(2610 + _core, 2810 + _core) - (_simTemp > 70 ? (_simTemp - 70) * 15 : 0);
        double memClock = _load < 5 ? 405 : 10501 + _mem;
        double voltCeiling = _voltLockMv > 0 ? _voltLockMv : 1100 + _voltOffset;
        double volt = _load < 5 ? 700 : Math.Clamp(750 + (coreClock - 1900) * 0.35 + _voltBoost * 1.5, 750, voltCeiling);
        double powerPct = _load < 5 ? 8 : Math.Min(_power, 30 + _load * 0.75);

        return new GpuTelemetry
        {
            CoreClockMhz = Math.Round(coreClock), MemoryClockMhz = Math.Round(memClock),
            TemperatureC = Math.Round(_simTemp, 1), HotSpotC = Math.Round(_simTemp + 12, 1),
            MemoryTemperatureC = Math.Round(_simTemp + 6, 1),
            VoltageMv = Math.Round(volt), PowerPercent = Math.Round(powerPct, 1),
            GpuLoadPercent = Math.Round(_load), MemoryLoadPercent = Math.Round(_load * 0.6),
            MemoryUsedMb = 1500 + _load * 60,
            FanPercent = Math.Round(fan), FanRpm = Math.Round(fan * 27),
            FanPercents = new[] { Math.Round(fan), Math.Round(fan) },
            FanRpms = new[] { Math.Round(fan * 27), Math.Round(fan * 26) },
            PerfState = _load < 5 ? "P8" : "P0",
            LimitReason = powerPct >= _power - 0.5 ? "Power" : (_simTemp >= _temp ? "Temperature" : "None")
        };
    }

    public GpuTuningState ReadTuningState(int gpuIndex) => new()
    {
        CoreOffsetMhz = _core, MemoryOffsetMhz = _mem, PowerLimitPercent = _power,
        TempLimitC = _temp, VoltageBoostPercent = _voltBoost, VoltageOffsetMv = _voltOffset,
        FanManual = _fanManual, FanPercent = _fan
    };

    public void SetCoreOffset(int gpuIndex, int offsetMhz) { Calls.Add(nameof(SetCoreOffset)); _core = offsetMhz; }
    public void SetMemoryOffset(int gpuIndex, int offsetMhz) { Calls.Add(nameof(SetMemoryOffset)); _mem = offsetMhz; }
    public void SetPowerLimit(int gpuIndex, int percent) => _power = percent;
    public void SetTempLimit(int gpuIndex, int celsius) => _temp = celsius;
    // Synthetic V/F curve: 8 points 700→1050 mV, 1500→2850 MHz (100 MHz per 50 mV). Deltas remembered.
    private readonly int[] _curveDeltaMhz = new int[8];
    private static int StockMv(int i) => 700 + i * 50;
    private static int StockMhz(int i) => 1500 + i * 150;

    public IReadOnlyList<VfCurveSample> ReadVfCurve(int gpuIndex)
    {
        var list = new List<VfCurveSample>(8);
        for (int i = 0; i < 8; i++)
            list.Add(new VfCurveSample(i, StockMv(i), StockMhz(i), StockMhz(i) + _curveDeltaMhz[i]));
        return list;
    }

    public void SetVfCurveTargets(int gpuIndex, IReadOnlyList<VfCurveSample> targets)
    {
        for (int i = 0; i < 8; i++) _curveDeltaMhz[i] = 0;
        foreach (var t in targets)
            if (t.Index >= 0 && t.Index < 8) _curveDeltaMhz[t.Index] = t.LiveMhz - StockMhz(t.Index);
    }

    /// <summary>
    /// Every mutating call, in order. Tests use it to pin down apply ordering — a setting that is
    /// written before something that clears it as a side effect looks fine but lands nowhere.
    /// </summary>
    public List<string> Calls { get; } = new();

    public void SetVoltageBoost(int gpuIndex, int percent) { Calls.Add(nameof(SetVoltageBoost)); _voltBoost = percent; }
    private int _voltLockMv;
    public void SetVoltageLock(int gpuIndex, int targetMv) { Calls.Add(nameof(SetVoltageLock)); _voltLockMv = targetMv; }
    public int ReadVoltageLockMv(int gpuIndex) => _voltLockMv;
    public void SetVoltageCurveOffset(int gpuIndex, int offsetMv, int extraClockMhz = 0)
    {
        Calls.Add(nameof(SetVoltageCurveOffset));
        _voltOffset = offsetMv;
        if (offsetMv != 0) _core = extraClockMhz;   // curve carries the core offset while undervolting
    }
    public void SetFanSpeed(int gpuIndex, int fanIndex, int percent) { _fanManual = true; _fan = percent; }
    public void SetFanAuto(int gpuIndex) => _fanManual = false;
    public void ResetToDefaults(int gpuIndex) { _core = 0; _mem = 0; _power = 100; _temp = 83; _voltBoost = 0; _voltOffset = 0; _voltLockMv = 0; _fanManual = false; }
    public string GetDiagnostics(int gpuIndex)
    {
        string fan = _fanManual ? _fan + "%" : "auto";
        return $"Simulated GPU — nothing real to dump.{Environment.NewLine}" +
               $"core={_core} mem={_mem} power={_power} temp={_temp} vboost={_voltBoost} fan={fan}{Environment.NewLine}";
    }
    public void Dispose() { }
}
