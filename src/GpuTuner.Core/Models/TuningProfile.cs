namespace GpuTuner.Core.Models;

public enum FanMode { Auto, Fixed, Curve }

/// <summary>A saved set of tuning values. Serialized to JSON.</summary>
public sealed class TuningProfile
{
    public string Name { get; set; } = "Default";
    public int Version { get; set; } = 1;

    /// <summary>GPU this profile was created for (informational; used to warn on mismatch).</summary>
    public string? GpuName { get; set; }

    public int CoreOffsetMhz { get; set; }
    public int MemoryOffsetMhz { get; set; }
    public int PowerLimitPercent { get; set; } = 100;
    public int TempLimitC { get; set; } = 83;
    public int VoltageBoostPercent { get; set; }

    /// <summary>Negative = undervolt. Caps the V/F curve this many mV below the stock maximum.</summary>
    public int VoltageOffsetMv { get; set; }
    /// <summary>Target ceiling for the core voltage rail, in mV. 0 = leave the rail at its default.</summary>
    public int VoltageRailMaxMv { get; set; }

    /// <summary>Target ceiling for the MSVDD rail, in mV. 0 = leave it at its default.</summary>
    public int MsvddRailMaxMv { get; set; }

    /// <summary>Target floors for the two rails, in mV. 0 = leave them at their defaults.</summary>
    public int VoltageRailFloorMv { get; set; }
    public int MsvddRailFloorMv { get; set; }

    /// <summary>Crossbar clock offset in MHz.</summary>
    public int XbarOffsetMhz { get; set; }

    /// <summary>SYS and video clock offsets in MHz. Gated with the crossbar; see XocEnabled.</summary>
    public int SysOffsetMhz { get; set; }
    public int VideoOffsetMhz { get; set; }

    /// <summary>
    /// Arms the three fields above - the two rails and the crossbar. They are off by default and
    /// written only when this is set, because they are the levers that can brown a card out rather
    /// than merely fail: a rail ceiling moves the roof the whole card runs under, and the crossbar
    /// is an undocumented domain the driver range-checks far more loosely. Everything else in this
    /// profile is bounded by ranges the driver itself reports, so it needs no gate.
    /// </summary>
    public bool XocEnabled { get; set; }

    /// <summary>
    /// Absolute core-voltage target in mV. When non-zero this drives both voltage boost and the
    /// curve flatten (see <see cref="VoltagePlan"/>) and takes precedence over the two fields above,
    /// which stay for older saved profiles and the CLI's --volt / --uv flags.
    /// </summary>
    public int TargetVoltageMv { get; set; }

    /// <summary>AMD: fans stop below the driver's idle threshold. Ignored on NVIDIA.</summary>
    public bool ZeroRpm { get; set; } = true;

    /// <summary>AMD: index into the vendor's memory timing presets (0 = default). Ignored on NVIDIA.</summary>
    public int MemoryTimingLevel { get; set; }

    public FanMode FanMode { get; set; } = FanMode.Auto;
    public int FixedFanPercent { get; set; } = 50;
    public FanCurve FanCurve { get; set; } = new();

    public DateTime ModifiedUtc { get; set; } = DateTime.UtcNow;

    public TuningProfile Clone() => new()
    {
        Name = Name,
        Version = Version,
        GpuName = GpuName,
        CoreOffsetMhz = CoreOffsetMhz,
        MemoryOffsetMhz = MemoryOffsetMhz,
        PowerLimitPercent = PowerLimitPercent,
        TempLimitC = TempLimitC,
        VoltageBoostPercent = VoltageBoostPercent,
        VoltageOffsetMv = VoltageOffsetMv,
        VoltageRailMaxMv = VoltageRailMaxMv,
        MsvddRailMaxMv = MsvddRailMaxMv,
        VoltageRailFloorMv = VoltageRailFloorMv,
        MsvddRailFloorMv = MsvddRailFloorMv,
        XbarOffsetMhz = XbarOffsetMhz,
        SysOffsetMhz = SysOffsetMhz,
        VideoOffsetMhz = VideoOffsetMhz,
        XocEnabled = XocEnabled,
        TargetVoltageMv = TargetVoltageMv,
        ZeroRpm = ZeroRpm,
        MemoryTimingLevel = MemoryTimingLevel,
        FanMode = FanMode,
        FixedFanPercent = FixedFanPercent,
        FanCurve = FanCurve.Clone(),
        ModifiedUtc = ModifiedUtc
    };

    public static TuningProfile Stock(GpuCapabilities caps, string? gpuName = null) => new()
    {
        Name = "Stock",
        GpuName = gpuName,
        CoreOffsetMhz = 0,
        PowerLimitPercent = caps.PowerLimitDefaultPercent,
        TempLimitC = caps.TempLimitDefaultC,
        // "Stock" for an absolute memory slider is the card's rated clock, not zero.
        MemoryOffsetMhz = caps.MemoryClockIsAbsolute ? caps.MemoryClockDefaultMhz : 0,
        ZeroRpm = caps.ZeroRpmDefault,
        FanMode = FanMode.Auto
    };

    /// <summary>Clamp every value into the ranges the driver reports.</summary>
    public void ClampTo(GpuCapabilities caps)
    {
        CoreOffsetMhz = Math.Clamp(CoreOffsetMhz, caps.CoreOffsetMinMhz, caps.CoreOffsetMaxMhz);
        MemoryOffsetMhz = Math.Clamp(MemoryOffsetMhz, caps.MemoryOffsetMinMhz, caps.MemoryOffsetMaxMhz);
        PowerLimitPercent = Math.Clamp(PowerLimitPercent, caps.PowerLimitMinPercent, caps.PowerLimitMaxPercent);
        TempLimitC = Math.Clamp(TempLimitC, caps.TempLimitMinC, caps.TempLimitMaxC);
        VoltageBoostPercent = Math.Clamp(VoltageBoostPercent, caps.VoltageBoostMinPercent, caps.VoltageBoostMaxPercent);
        VoltageOffsetMv = Math.Clamp(VoltageOffsetMv, caps.VoltageOffsetMinMv, caps.VoltageOffsetMaxMv);
        // 0 means "leave the rail alone"; only a real request gets clamped into the offered range.
        if (VoltageRailMaxMv > 0 && caps.VoltageRailMaxMv > 0)
            VoltageRailMaxMv = Math.Clamp(VoltageRailMaxMv, caps.VoltageRailMinMv, caps.VoltageRailMaxMv);
        if (MsvddRailMaxMv > 0 && caps.MsvddRailMaxMv > 0)
            MsvddRailMaxMv = Math.Clamp(MsvddRailMaxMv, caps.MsvddRailMinMv, caps.MsvddRailMaxMv);
        if (VoltageRailFloorMv > 0 && caps.VoltageRailFloorMaxMv > 0)
            VoltageRailFloorMv = Math.Clamp(VoltageRailFloorMv, caps.VoltageRailFloorMinMv, caps.VoltageRailFloorMaxMv);
        if (MsvddRailFloorMv > 0 && caps.MsvddRailFloorMaxMv > 0)
            MsvddRailFloorMv = Math.Clamp(MsvddRailFloorMv, caps.MsvddRailFloorMinMv, caps.MsvddRailFloorMaxMv);
        if (caps.CanSetXbarOffset)
            XbarOffsetMhz = Math.Clamp(XbarOffsetMhz, caps.XbarOffsetMinMhz, caps.XbarOffsetMaxMhz);
        if (caps.CanSetSysOffset)
            SysOffsetMhz = Math.Clamp(SysOffsetMhz, caps.SysOffsetMinMhz, caps.SysOffsetMaxMhz);
        if (caps.CanSetVideoOffset)
            VideoOffsetMhz = Math.Clamp(VideoOffsetMhz, caps.VideoOffsetMinMhz, caps.VideoOffsetMaxMhz);
        if (TargetVoltageMv > 0 && caps.MaxVoltageMv > 0)
            TargetVoltageMv = Math.Clamp(TargetVoltageMv, caps.MinVoltageMv, caps.MaxVoltageMv);
        FixedFanPercent = Math.Clamp(FixedFanPercent, caps.FanMinPercent, caps.FanMaxPercent);
        if (caps.MemoryTimingOptions.Count > 0)
            MemoryTimingLevel = Math.Clamp(MemoryTimingLevel, 0, caps.MemoryTimingOptions.Count - 1);
        FanCurve.Normalize();
    }
}
