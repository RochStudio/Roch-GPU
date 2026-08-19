namespace GpuTuner.Core.Models;

/// <summary>One editable point of the voltage/frequency curve, in display units (mV, MHz).</summary>
public readonly record struct VfCurveSample(int Index, int VoltageMv, int StockMhz, int LiveMhz);

/// <summary>Static identity of a GPU as discovered by a backend.</summary>
public sealed record GpuDevice(
    int Index,
    string Name,
    string Vendor,
    string BusId,
    string DriverVersion,
    long VramMegabytes,
    string BiosVersion);

/// <summary>What the backend allows us to change, plus the driver-imposed ranges.</summary>
public sealed record GpuCapabilities
{
    public bool CanSetCoreOffset { get; init; }
    public bool CanSetMemoryOffset { get; init; }
    public bool CanSetPowerLimit { get; init; }
    public bool CanSetTempLimit { get; init; }
    public bool CanSetFanSpeed { get; init; }
    public bool CanReadVoltage { get; init; }
    public bool CanSetVoltageBoost { get; init; }

    /// <summary>Core voltage boost range in percent (0 = stock V/F curve, higher = more voltage headroom).</summary>
    public int VoltageBoostMinPercent { get; init; }
    public int VoltageBoostMaxPercent { get; init; } = 100;

    /// <summary>True when the V/F curve can be edited — the only real undervolt path on NVIDIA.</summary>
    public bool CanSetVoltageCurve { get; init; }

    /// <summary>Negative-only voltage offset range in mV (0 = stock, -150 = cap 150 mV below stock max).</summary>
    public int VoltageOffsetMinMv { get; init; } = -300;
    public int VoltageOffsetMaxMv { get; init; }

    /// <summary>Highest voltage on the stock V/F curve, in mV. 0 when unknown.</summary>
    public int StockMaxVoltageMv { get; init; }

    /// <summary>Why the V/F curve is unavailable, when CanSetVoltageCurve is false. Empty if it works.</summary>
    public string CurveUnavailableReason { get; init; } = "";

    /// <summary>Absolute core-voltage window for the single voltage slider, in mV.</summary>
    public int MinVoltageMv { get; init; } = 850;
    /// <summary>Stock curve top plus full voltage-boost headroom — the highest the card will go.</summary>
    public int MaxVoltageMv { get; init; }
    /// <summary>True when either lever (boost up, curve flatten down) is available.</summary>
    public bool CanSetVoltage => (CanSetVoltageCurve || CanSetVoltageBoost) && StockMaxVoltageMv > 0;

    /// <summary>Core clock offset range in MHz (driver limits).</summary>
    public int CoreOffsetMinMhz { get; init; } = -500;
    public int CoreOffsetMaxMhz { get; init; } = 500;

    /// <summary>Memory clock offset range in MHz (driver limits).</summary>
    public int MemoryOffsetMinMhz { get; init; } = -1000;
    public int MemoryOffsetMaxMhz { get; init; } = 1500;

    /// <summary>Power limit range in percent of TDP.</summary>
    public int PowerLimitMinPercent { get; init; } = 100;
    public int PowerLimitMaxPercent { get; init; } = 100;
    public int PowerLimitDefaultPercent { get; init; } = 100;

    /// <summary>Temperature limit range in °C.</summary>
    public int TempLimitMinC { get; init; } = 65;
    public int TempLimitMaxC { get; init; } = 90;
    public int TempLimitDefaultC { get; init; } = 83;

    /// <summary>Fan speed range in percent.</summary>
    public int FanMinPercent { get; init; } = 0;
    public int FanMaxPercent { get; init; } = 100;

    /// <summary>Number of independently controllable fans (0 if fan control unsupported).</summary>
    public int FanCount { get; init; }

    /// <summary>
    /// True when the core power rail's own voltage ceiling can be moved. This is a different lever
    /// from the boost percentage and from the V/F cap: it raises or lowers the rail's maximum
    /// outright, rather than asking the boost algorithm for more headroom within it.
    /// </summary>
    public bool CanSetVoltageRail { get; init; }
    /// <summary>Rail ceiling range offered, in mV — the driver's, narrowed to a sane one.</summary>
    public int VoltageRailMinMv { get; init; }
    public int VoltageRailMaxMv { get; init; }
    /// <summary>The rail's ceiling with no offset applied, i.e. what "default" restores.</summary>
    public int VoltageRailStockMaxMv { get; init; }
    /// <summary>Floor range offered for the core rail, and the value it is configured at.</summary>
    public int VoltageRailFloorMinMv { get; init; }
    public int VoltageRailFloorMaxMv { get; init; }
    public int VoltageRailStockFloorMv { get; init; }

    /// <summary>
    /// True when the crossbar (interconnect) clock takes an offset. Not a public clock domain —
    /// PStates20 refuses it — so it is detected by asking its own control family.
    /// </summary>
    /// <summary>
    /// MSVDD is the rail feeding the crossbar, SYS and video domains — a separate supply from NVVDD,
    /// with its own ceiling.
    /// </summary>
    public bool CanSetMsvddRail { get; init; }
    public int MsvddRailMinMv { get; init; }
    public int MsvddRailMaxMv { get; init; }
    public int MsvddRailStockMaxMv { get; init; }
    public int MsvddRailFloorMinMv { get; init; }
    public int MsvddRailFloorMaxMv { get; init; }
    public int MsvddRailStockFloorMv { get; init; }

    public bool CanSetXbarOffset { get; init; }
    public int XbarOffsetMinMhz { get; init; }
    public int XbarOffsetMaxMhz { get; init; }

    // ---------------- vendor differences the UI has to reflect ----------------

    /// <summary>
    /// How this vendor lets you move the core voltage. NVIDIA exposes an editable V/F curve, so the
    /// slider is an absolute target; AMD exposes a single negative offset instead.
    /// </summary>
    public VoltageControlStyle VoltageStyle { get; init; } = VoltageControlStyle.Absolute;

    /// <summary>True when the memory slider is an absolute clock (AMD) rather than an offset (NVIDIA).</summary>
    public bool MemoryClockIsAbsolute { get; init; }
    /// <summary>Stock memory clock in MHz, when the slider is absolute.</summary>
    public int MemoryClockDefaultMhz { get; init; }

    /// <summary>True when the power limit is a +/- offset around stock rather than a percentage of TDP.</summary>
    public bool PowerLimitIsOffset { get; init; }

    /// <summary>Zero RPM (fan stop at idle) can be turned on and off.</summary>
    public bool CanSetZeroRpm { get; init; }
    public bool ZeroRpmDefault { get; init; } = true;

    /// <summary>Memory timing presets are selectable (AMD's "fast timing").</summary>
    public bool CanSetMemoryTiming { get; init; }
    public IReadOnlyList<string> MemoryTimingOptions { get; init; } = Array.Empty<string>();

    /// <summary>
    /// True when the fan curve is executed by the driver rather than by this app's polling loop.
    /// A hardware curve keeps running after the app exits, and is limited to
    /// <see cref="FanCurvePoints"/> points.
    /// </summary>
    public bool FanCurveIsHardware { get; init; }
    public int FanCurvePoints { get; init; }
    public int FanCurveMinTempC { get; init; } = 0;
    public int FanCurveMaxTempC { get; init; } = 110;
}

/// <summary>Which shape the core-voltage control takes on this vendor.</summary>
public enum VoltageControlStyle
{
    /// <summary>No voltage control at all.</summary>
    None,
    /// <summary>Absolute target in mV (NVIDIA: cap or boost against the V/F curve).</summary>
    Absolute,
    /// <summary>Signed offset in mV applied to the whole curve (AMD).</summary>
    Offset
}

/// <summary>Instantaneous sensor readout.</summary>
public sealed record GpuTelemetry
{
    public DateTime Timestamp { get; init; } = DateTime.UtcNow;
    public double CoreClockMhz { get; init; }
    public double MemoryClockMhz { get; init; }
    public double TemperatureC { get; init; }
    public double HotSpotC { get; init; }          // NaN when unavailable
    public double MemoryTemperatureC { get; init; } // NaN when unavailable
    public double VoltageMv { get; init; }          // NaN when unavailable
    public double PowerPercent { get; init; }       // % of TDP (0 when the vendor reports watts instead)
    public double PowerWatts { get; init; }         // board power draw; 0 when unavailable
    public double GpuLoadPercent { get; init; }
    public double MemoryLoadPercent { get; init; }
    public double MemoryUsedMb { get; init; }
    public double FanPercent { get; init; }
    public double FanRpm { get; init; }
    public double[] FanPercents { get; init; } = Array.Empty<double>();
    public double[] FanRpms { get; init; } = Array.Empty<double>();
    public string PerfState { get; init; } = "";
    public string LimitReason { get; init; } = "";  // Power / Thermal / Voltage / None
}

/// <summary>Current applied tuning as read back from the driver.</summary>
public sealed record GpuTuningState
{
    public int CoreOffsetMhz { get; init; }
    public int MemoryOffsetMhz { get; init; }
    public int PowerLimitPercent { get; init; } = 100;
    public int TempLimitC { get; init; } = 83;
    public int VoltageBoostPercent { get; init; }
    /// <summary>Inferred from the live curve: how far below stock max the curve is flattened. 0 = not flattened.</summary>
    public int VoltageOffsetMv { get; init; }
    /// <summary>Current ceiling of the core voltage rail in mV; 0 when the card doesn't expose it.</summary>
    public int VoltageRailMaxMv { get; init; }
    /// <summary>Current ceiling of the MSVDD rail in mV; 0 when the card doesn't expose it.</summary>
    public int MsvddRailMaxMv { get; init; }
    /// <summary>Current floors of the two rails in mV; 0 when unavailable.</summary>
    public int VoltageRailFloorMv { get; init; }
    public int MsvddRailFloorMv { get; init; }
    /// <summary>Crossbar clock offset currently applied, in MHz.</summary>
    public int XbarOffsetMhz { get; init; }
    public bool FanManual { get; init; }
    public int FanPercent { get; init; }

    /// <summary>
    /// Fan mode as read back out of the hardware, for a backend that can tell the three apart.
    /// Null when the backend cannot, in which case <see cref="FanManual"/> is the only signal — a
    /// hardware curve is indistinguishable from auto through that flag alone.
    /// </summary>
    public FanMode? DetectedFanMode { get; init; }

    /// <summary>AMD: fans stop entirely below the driver's idle threshold.</summary>
    public bool ZeroRpm { get; init; } = true;
    /// <summary>AMD: index into <see cref="GpuCapabilities.MemoryTimingOptions"/>.</summary>
    public int MemoryTimingLevel { get; init; }
}
