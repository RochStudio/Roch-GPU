using System.Runtime.InteropServices;
using GpuTuner.Core.Models;

namespace GpuTuner.Core.Backends.Amd;

/// <summary>
/// AMD backend, Overdrive 8 over ADL. Covers RDNA and newer.
///
/// Everything it can do is discovered at runtime rather than hardcoded: the driver hands back a
/// capability bitmask plus a per-feature table of min/max/default, and a feature whose min equals
/// its max is one this card does not expose. On an RX 9070 XT that leaves core-clock offset,
/// voltage offset, memory clock, power limit, memory timing, zero RPM and a 5-point fan curve —
/// and correctly hides the V/F curve and temperature limit, which RDNA 4 does not offer.
/// </summary>
public sealed class AdlBackend : IGpuBackend
{
    private IntPtr _context;
    private readonly List<GpuDevice> _devices = new();
    private readonly List<int> _adapterIndex = new();     // device index -> ADL adapter index
    // Per adapter: a second AMD card has its own limits, and clamping one card's writes to the
    // other's ranges would silently truncate them.
    private readonly Dictionary<int, int> _caps = new();
    private readonly Dictionary<int, Od8Range[]> _rangesByGpu = new();
    private readonly object _gate = new();

    /// <summary>min/max/default for one OD8 feature. Unsupported features report min == max.</summary>
    private readonly record struct Od8Range(int Min, int Max, int Default)
    {
        public bool Supported => Min != Max;
    }

    public string BackendName => "AMD (ADL Overdrive 8)";
    public IReadOnlyList<GpuDevice> Devices => _devices;

    public void Initialize()
    {
        // Idempotent: BackendFactory initialises while probing, then TuningService initialises again.
        // A second ADL2_Main_Control_Create would leak a context.
        if (_context != IntPtr.Zero) return;
        int rc;
        try
        {
            rc = AdlNative.ADL2_Main_Control_Create(AdlNative.Allocator, 1, out _context);
        }
        catch (DllNotFoundException e)
        {
            throw new GpuBackendException("atiadlxx.dll not found — no AMD driver on this machine.", e);
        }
        catch (EntryPointNotFoundException e)
        {
            throw new GpuBackendException("This AMD driver is too old for the Overdrive 8 API.", e);
        }
        if (rc != AdlNative.AdlOk || _context == IntPtr.Zero)
            throw new GpuBackendException($"ADL2_Main_Control_Create failed: {AdlNative.Describe(rc)}.");

        EnumerateAdapters();
        if (_devices.Count == 0)
            throw new GpuBackendException("ADL initialised but reported no AMD adapters.");

        LoadRanges(0);
    }

    private void EnumerateAdapters()
    {
        if (AdlNative.ADL2_Adapter_NumberOfAdapters_Get(_context, out int num) != AdlNative.AdlOk || num <= 0)
            return;

        int sz = Marshal.SizeOf<AdlNative.AdapterInfo>();
        IntPtr buf = Marshal.AllocHGlobal(sz * num);
        try
        {
            if (AdlNative.ADL2_Adapter_AdapterInfo_Get(_context, buf, sz * num) != AdlNative.AdlOk) return;

            // ADL reports one entry per display output, so the same GPU appears several times.
            var seen = new HashSet<string>();
            for (int i = 0; i < num; i++)
            {
                var ai = Marshal.PtrToStructure<AdlNative.AdapterInfo>(IntPtr.Add(buf, i * sz));
                if (ai.iVendorID != AdlNative.AmdVendorId || ai.iExist == 0) continue;
                string key = $"{ai.iBusNumber}.{ai.iDeviceNumber}.{ai.iFunctionNumber}";
                if (!seen.Add(key)) continue;

                _devices.Add(new GpuDevice(
                    Index: _devices.Count,
                    Name: (ai.strAdapterName ?? "AMD GPU").Trim(),
                    Vendor: "AMD",
                    BusId: $"PCI bus {ai.iBusNumber}, device {ai.iDeviceNumber}",
                    DriverVersion: ReadDriverVersion(ai.strDriverPath),
                    VramMegabytes: ReadVramMegabytes(ai.iAdapterIndex),
                    BiosVersion: ReadBiosVersion(ai.iAdapterIndex)));
                _adapterIndex.Add(ai.iAdapterIndex);
            }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    // ---- identity ---------------------------------------------------------
    //
    // These three are best-effort. Each entry point is optional on a given driver, and a missing one
    // throws EntryPointNotFoundException from the P/Invoke rather than returning a code — so each is
    // wrapped and simply yields "unknown", which the UI then omits instead of printing "0 GB".

    private static string AnsiAt(IntPtr buffer, int offset)
    {
        string? s = Marshal.PtrToStringAnsi(IntPtr.Add(buffer, offset));
        return string.IsNullOrWhiteSpace(s) ? "" : s.Trim();
    }

    private T WithBuffer<T>(int bytes, Func<IntPtr, T> body, T fallback)
    {
        IntPtr buf = Marshal.AllocHGlobal(bytes);
        try
        {
            for (int i = 0; i < bytes; i += 4) Marshal.WriteInt32(buf, i, 0);
            return body(buf);
        }
        catch (EntryPointNotFoundException) { return fallback; }
        catch (DllNotFoundException) { return fallback; }
        catch (Exception) { return fallback; }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>
    /// Driver version. ADL is asked first, and the adapter's own driver registry key is the fallback
    /// — that key is where AMD records both the Windows driver version and the Adrenalin release
    /// number, and it answers on drivers whose ADL version export returns nothing.
    /// </summary>
    private string ReadDriverVersion(string? driverPath)
    {
        string fromAdl = WithBuffer(4096, buf =>
        {
            // ADLVersionsInfoX2 and ADLVersionsInfo both open with char strDriverVer[256], so the
            // layout difference after that field does not matter here.
            int rc;
            try { rc = AdlNative.ADL2_Graphics_VersionsX2_Get(_context, buf); }
            catch (EntryPointNotFoundException) { rc = AdlNative.ADL2_Graphics_Versions_Get(_context, buf); }
            if (rc != AdlNative.AdlOk) return "";
            string driver = AnsiAt(buf, 0);
            string catalyst = AnsiAt(buf, 256);
            return catalyst.Length > 0 ? catalyst : driver;
        }, "");
        if (fromAdl.Length > 0) return fromAdl;

        return OperatingSystem.IsWindows() ? ReadDriverVersionFromRegistry(driverPath) : "";
    }

    /// <summary>
    /// AdapterInfo.strDriverPath is an NT-style path to the adapter's driver key, e.g.
    /// <c>\Registry\Machine\System\CurrentControlSet\Control\Class\{4d36e968-...}\0000</c>.
    /// AMD writes RadeonSoftwareVersion ("25.3.1") and DriverVersion ("32.0.23033.1002") there.
    /// </summary>
    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static string ReadDriverVersionFromRegistry(string? driverPath)
    {
        static string? Read(string subKey)
        {
            try
            {
                using var key = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(subKey);
                if (key == null) return null;
                string? radeon = key.GetValue("RadeonSoftwareVersion") as string;   // the friendly one
                if (!string.IsNullOrWhiteSpace(radeon)) return radeon.Trim();
                string? driver = key.GetValue("DriverVersion") as string;
                return string.IsNullOrWhiteSpace(driver) ? null : driver.Trim();
            }
            catch { return null; }
        }

        if (!string.IsNullOrWhiteSpace(driverPath))
        {
            string p = driverPath.Replace('/', '\\').TrimStart('\\');
            const string prefix = "Registry\\Machine\\";
            if (p.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) p = p[prefix.Length..];
            if (Read(p) is { } hit) return hit;
        }

        // Fallback: walk the display class and take the first AMD adapter that names itself.
        const string displayClass = @"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
        try
        {
            using var cls = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(displayClass);
            if (cls != null)
            {
                foreach (var name in cls.GetSubKeyNames())
                {
                    if (name.Length != 4) continue;                    // 0000, 0001, ...
                    using var sub = cls.OpenSubKey(name);
                    string provider = sub?.GetValue("ProviderName") as string ?? "";
                    if (!provider.Contains("Advanced Micro Devices", StringComparison.OrdinalIgnoreCase) &&
                        !provider.Contains("AMD", StringComparison.OrdinalIgnoreCase)) continue;
                    if (Read($"{displayClass}\\{name}") is { } hit) return hit;
                }
            }
        }
        catch { }
        return "";
    }

    /// <summary>vBIOS version string. Empty when unavailable.</summary>
    private string ReadBiosVersion(int adapterIndex) => WithBuffer(1024, buf =>
        AdlNative.ADL2_Adapter_VideoBiosInfo_Get(_context, adapterIndex, buf) == AdlNative.AdlOk
            ? AnsiAt(buf, 256)                       // {partNumber[256], version[256], date[256]}
            : "", "");

    /// <summary>VRAM in MB, or 0 when the driver does not report a believable figure.</summary>
    private long ReadVramMegabytes(int adapterIndex) => WithBuffer(2048, buf =>
    {
        int rc;
        try { rc = AdlNative.ADL2_Adapter_MemoryInfo2_Get(_context, adapterIndex, buf); }
        catch (EntryPointNotFoundException) { rc = AdlNative.ADL2_Adapter_MemoryInfo_Get(_context, adapterIndex, buf); }
        if (rc != AdlNative.AdlOk) return 0L;

        long bytes = Marshal.ReadInt64(buf, 0);      // both structs open with long long iMemorySize
        long mb = bytes / (1024 * 1024);
        // Sanity-check rather than trust it: a wrong struct shape shows up as a nonsense number,
        // and "unknown" is a better answer than "1048576 GB".
        return mb is >= 256 and <= 262144 ? mb : 0L;
    }, 0L);

    private int Adapter(int gpuIndex) =>
        gpuIndex >= 0 && gpuIndex < _adapterIndex.Count
            ? _adapterIndex[gpuIndex]
            : throw new GpuBackendException($"No AMD adapter at index {gpuIndex}.");

    // ------------------------------------------------------------------ OD8 tables

    /// <summary>Read the capability mask and the per-feature ranges. Cached; they do not change.</summary>
    private void LoadRanges(int gpuIndex)
    {
        int caps = 0, count = AdlNative.Od8FeatureCount;
        IntPtr list = IntPtr.Zero;
        int rc = AdlNative.ADL2_Overdrive8_Init_SettingX2_Get(_context, Adapter(gpuIndex), ref caps, ref count, ref list);
        if (rc != AdlNative.AdlOk || list == IntPtr.Zero)
            throw new GpuBackendException($"Could not read the Overdrive 8 feature table: {AdlNative.Describe(rc)}.");

        try
        {
            int n = Math.Min(count <= 0 ? AdlNative.Od8FeatureCount : count, AdlNative.Od8Count);
            var ranges = new Od8Range[AdlNative.Od8Count];
            for (int i = 0; i < n; i++)
            {
                IntPtr e = IntPtr.Add(list, i * 16);      // featureID, min, max, default
                ranges[i] = new Od8Range(Marshal.ReadInt32(e, 4), Marshal.ReadInt32(e, 8), Marshal.ReadInt32(e, 12));
            }
            _caps[gpuIndex] = caps;
            _rangesByGpu[gpuIndex] = ranges;
        }
        finally { Marshal.FreeCoTaskMem(list); }   // the driver allocated it through our allocator
    }

    private Od8Range[] Ranges(int gpuIndex)
    {
        if (!_rangesByGpu.TryGetValue(gpuIndex, out var r)) { LoadRanges(gpuIndex); r = _rangesByGpu[gpuIndex]; }
        return r;
    }

    private Od8Range Range(int gpuIndex, Od8Id id)
    {
        var r = Ranges(gpuIndex);
        return (int)id < r.Length ? r[(int)id] : default;
    }

    private bool Has(int gpuIndex, Od8Feature f)
    {
        Ranges(gpuIndex);                                   // ensures _caps is populated
        return _caps.TryGetValue(gpuIndex, out int c) && (c & (int)f) == (int)f;
    }

    /// <summary>Current values for every OD8 feature, indexed by <see cref="Od8Id"/>.</summary>
    private int[] ReadCurrent(int gpuIndex)
    {
        int count = AdlNative.Od8FeatureCount;
        IntPtr list = IntPtr.Zero;
        int rc = AdlNative.ADL2_Overdrive8_Current_SettingX2_Get(_context, Adapter(gpuIndex), ref count, ref list);
        var values = new int[AdlNative.Od8Count];
        if (rc != AdlNative.AdlOk || list == IntPtr.Zero) return values;
        try
        {
            int n = Math.Min(count, AdlNative.Od8Count);
            for (int i = 0; i < n; i++) values[i] = Marshal.ReadInt32(list, i * 4);
            return values;
        }
        finally { Marshal.FreeCoTaskMem(list); }
    }

    /// <summary>
    /// Write one or more OD8 settings. Every slot is sent every time — the driver takes the whole
    /// table — but only the ones named here are flagged "requested", so the rest keep their values.
    /// </summary>
    private void Write(int gpuIndex, params (Od8Id id, int value)[] settings)
    {
        if (settings.Length == 0) return;

        // ADLOD8SetSetting: int count; { int value; int requested; int reset; } table[Od8Count]
        const int setSize = 4 + AdlNative.Od8Count * 12;
        const int curSize = 4 + AdlNative.Od8Count * 4;
        IntPtr set = Marshal.AllocHGlobal(setSize);
        IntPtr cur = Marshal.AllocHGlobal(curSize);
        try
        {
            for (int i = 0; i < setSize; i += 4) Marshal.WriteInt32(set, i, 0);
            for (int i = 0; i < curSize; i += 4) Marshal.WriteInt32(cur, i, 0);
            Marshal.WriteInt32(set, 0, AdlNative.Od8FeatureCount);
            Marshal.WriteInt32(cur, 0, AdlNative.Od8FeatureCount);

            foreach (var (id, value) in settings)
            {
                int o = 4 + (int)id * 12;
                Marshal.WriteInt32(set, o, value);          // value
                Marshal.WriteInt32(set, o + 4, 1);          // requested
                Marshal.WriteInt32(set, o + 8, 0);          // reset — leaving this 0 is deliberate:
                                                            // some cards refuse the write when both
                                                            // reset and requested are set.
            }

            int rc = AdlNative.ADL2_Overdrive8_Setting_Set(_context, Adapter(gpuIndex), set, cur);
            if (rc != AdlNative.AdlOk)
                throw new GpuBackendException(
                    $"The driver rejected the tuning write: {AdlNative.Describe(rc)}. " +
                    "AMD Software must be set to a Default or Custom tuning preset for Overdrive writes to apply.");
        }
        finally { Marshal.FreeHGlobal(set); Marshal.FreeHGlobal(cur); }
    }

    // ------------------------------------------------------------------ capabilities

    public GpuCapabilities GetCapabilities(int gpuIndex)
    {
        var core = Range(gpuIndex, Od8Id.GfxClkFMax);
        var mem = Range(gpuIndex, Od8Id.UClkFMax);
        var power = Range(gpuIndex, Od8Id.PowerPercentage);
        var volt = Range(gpuIndex, Od8Id.OdVoltage);
        var fanT1 = Range(gpuIndex, Od8Id.FanCurveTemperature1);
        var fanS1 = Range(gpuIndex, Od8Id.FanCurveSpeed1);

        return new GpuCapabilities
        {
            CanSetCoreOffset = Has(gpuIndex, Od8Feature.GfxClkLimits) && core.Supported,
            CoreOffsetMinMhz = core.Supported ? core.Min : 0,
            CoreOffsetMaxMhz = core.Supported ? core.Max : 0,

            // RDNA reports an absolute memory clock, not an offset — the UI reads MemoryClockIsAbsolute.
            CanSetMemoryOffset = Has(gpuIndex, Od8Feature.UClkMax) && mem.Supported,
            MemoryOffsetMinMhz = mem.Supported ? mem.Min : 0,
            MemoryOffsetMaxMhz = mem.Supported ? mem.Max : 0,
            MemoryClockIsAbsolute = true,
            MemoryClockDefaultMhz = mem.Default,

            CanSetPowerLimit = Has(gpuIndex, Od8Feature.PowerLimit) && power.Supported,
            PowerLimitMinPercent = power.Supported ? power.Min : 0,
            PowerLimitMaxPercent = power.Supported ? power.Max : 0,
            PowerLimitDefaultPercent = power.Default,
            PowerLimitIsOffset = true,

            // Locked on RDNA 4: the driver owns the thermal limit.
            CanSetTempLimit = Has(gpuIndex, Od8Feature.TemperatureSystem) && Range(gpuIndex, Od8Id.OperatingTempMax).Supported,
            TempLimitMinC = Range(gpuIndex, Od8Id.OperatingTempMax).Min,
            TempLimitMaxC = Range(gpuIndex, Od8Id.OperatingTempMax).Max,
            TempLimitDefaultC = Range(gpuIndex, Od8Id.OperatingTempMax).Default,

            // Voltage is a negative offset here, not a point on an editable curve.
            VoltageStyle = Has(gpuIndex, Od8Feature.OdVoltageLimit) && volt.Supported
                ? VoltageControlStyle.Offset
                : VoltageControlStyle.None,
            VoltageOffsetMinMv = volt.Supported ? volt.Min : 0,
            VoltageOffsetMaxMv = volt.Supported ? volt.Max : 0,
            CanSetVoltageCurve = false,
            CurveUnavailableReason = Has(gpuIndex, Od8Feature.GfxClkCurve)
                ? ""
                : "this architecture exposes a voltage offset instead of an editable V/F curve",
            CanReadVoltage = true,

            CanSetZeroRpm = Has(gpuIndex, Od8Feature.FanZeroRpmControl) && Range(gpuIndex, Od8Id.FanZeroRpmControl).Supported,
            ZeroRpmDefault = Range(gpuIndex, Od8Id.FanZeroRpmControl).Default != 0,

            CanSetMemoryTiming = Has(gpuIndex, Od8Feature.MemoryTimingTune) && Range(gpuIndex, Od8Id.AcTiming).Supported,
            MemoryTimingOptions = new[] { "Default", "Fast timing" },

            // A hardware curve: exactly five points, and the driver runs it — no polling loop needed.
            CanSetFanSpeed = Has(gpuIndex, Od8Feature.FanCurve) && fanT1.Supported,
            FanCurveIsHardware = true,
            FanCurvePoints = 5,
            FanCurveMinTempC = fanT1.Supported ? fanT1.Min : 25,
            FanCurveMaxTempC = fanT1.Supported ? fanT1.Max : 100,
            FanMinPercent = fanS1.Supported ? fanS1.Min : 0,
            FanMaxPercent = fanS1.Supported ? fanS1.Max : 100,
            FanCount = 1
        };
    }

    // ------------------------------------------------------------------ telemetry

    public GpuTelemetry ReadTelemetry(int gpuIndex)
    {
        const int size = 4 + 256 * 8;                 // int size; { int supported; int value; }[256]
        IntPtr buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            if (AdlNative.ADL2_New_QueryPMLogData_Get(Live(), Adapter(gpuIndex), buf) != AdlNative.AdlOk)
                return new GpuTelemetry();

            double S(PmLog id)
            {
                int o = 4 + (int)id * 8;
                return Marshal.ReadInt32(buf, o) != 0 ? Marshal.ReadInt32(buf, o + 4) : double.NaN;
            }
            static double OrZero(double v) => double.IsNaN(v) ? 0 : v;

            double fanPct = S(PmLog.FanPercent);
            double watts = S(PmLog.BoardPowerW);
            if (double.IsNaN(watts)) watts = S(PmLog.AsicPowerW);

            return new GpuTelemetry
            {
                CoreClockMhz = OrZero(S(PmLog.CoreClockMhz)),
                MemoryClockMhz = OrZero(S(PmLog.MemoryClockMhz)),
                TemperatureC = OrZero(S(PmLog.TemperatureEdge)),
                HotSpotC = S(PmLog.TemperatureHotspot),
                MemoryTemperatureC = S(PmLog.TemperatureMemory),
                VoltageMv = S(PmLog.GfxVoltageMv),
                PowerWatts = OrZero(watts),
                GpuLoadPercent = OrZero(S(PmLog.ActivityGfx)),
                MemoryLoadPercent = OrZero(S(PmLog.ActivityMem)),
                FanPercent = OrZero(fanPct),
                FanRpm = OrZero(S(PmLog.FanRpm)),
                FanPercents = new[] { OrZero(fanPct) },
                FanRpms = new[] { OrZero(S(PmLog.FanRpm)) },
                LimitReason = ThrottleText(S(PmLog.ThrottlerStatus))
            };
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    private static string ThrottleText(double status)
    {
        if (double.IsNaN(status) || status == 0) return "None";
        int s = (int)status;
        var hits = new List<string>();
        if ((s & 1) != 0) hits.Add("Power");
        if ((s & 2) != 0) hits.Add("Thermal");
        if ((s & 4) != 0) hits.Add("Current");
        return hits.Count == 0 ? "Other" : string.Join(" + ", hits);
    }

    public GpuTuningState ReadTuningState(int gpuIndex)
    {
        var v = ReadCurrent(gpuIndex);
        var fan = ReadFanMode(gpuIndex, v);
        return new GpuTuningState
        {
            CoreOffsetMhz = v[(int)Od8Id.GfxClkFMax],
            MemoryOffsetMhz = v[(int)Od8Id.UClkFMax],
            PowerLimitPercent = v[(int)Od8Id.PowerPercentage],
            TempLimitC = v[(int)Od8Id.OperatingTempMax],
            VoltageOffsetMv = v[(int)Od8Id.OdVoltage],
            ZeroRpm = v[(int)Od8Id.FanZeroRpmControl] != 0,
            MemoryTimingLevel = v[(int)Od8Id.AcTiming],
            DetectedFanMode = fan.Mode,
            // Reported as manual only for a fixed duty. A hardware curve is the driver's to run, and
            // callers that only see this flag treat manual as "a single percentage is in force".
            FanManual = fan.Mode == FanMode.Fixed,
            FanPercent = fan.Percent
        };
    }

    private static readonly Od8Id[] FanCurveTempIds =
    {
        Od8Id.FanCurveTemperature1, Od8Id.FanCurveTemperature2, Od8Id.FanCurveTemperature3,
        Od8Id.FanCurveTemperature4, Od8Id.FanCurveTemperature5
    };

    private static readonly Od8Id[] FanCurveSpeedIds =
    {
        Od8Id.FanCurveSpeed1, Od8Id.FanCurveSpeed2, Od8Id.FanCurveSpeed3,
        Od8Id.FanCurveSpeed4, Od8Id.FanCurveSpeed5
    };

    /// <summary>Pull the live fan table and its defaults, skipping points this card doesn't expose.</summary>
    private (FanMode Mode, int Percent) ReadFanMode(int gpuIndex, int[] cur)
    {
        List<int> temps = new(), tempDefaults = new(), speeds = new(), speedDefaults = new();
        for (int i = 0; i < FanCurveSpeedIds.Length; i++)
        {
            var sr = Range(gpuIndex, FanCurveSpeedIds[i]);
            if (!sr.Supported) continue;
            speeds.Add(cur[(int)FanCurveSpeedIds[i]]);
            speedDefaults.Add(sr.Default);

            var tr = Range(gpuIndex, FanCurveTempIds[i]);
            if (!tr.Supported) continue;
            temps.Add(cur[(int)FanCurveTempIds[i]]);
            tempDefaults.Add(tr.Default);
        }
        return InferFanMode(temps.ToArray(), speeds.ToArray(), tempDefaults.ToArray(), speedDefaults.ToArray());
    }

    /// <summary>
    /// There is no "manual fan" flag on this driver — the five-point hardware curve is the only fan
    /// mechanism, so the mode has to be inferred from the table itself:
    ///
    /// <list type="bullet">
    /// <item>the driver's own default table means nothing has overridden it — auto;</item>
    /// <item>five equal duties is what <see cref="SetFanSpeed"/> writes for a fixed speed;</item>
    /// <item>anything else is a curve.</item>
    /// </list>
    ///
    /// A hand-drawn flat curve therefore reads back as fixed, which is precisely what it is once it
    /// reaches the hardware. Static and array-based so it can be tested without a driver.
    /// </summary>
    public static (FanMode Mode, int Percent) InferFanMode(
        int[] temps, int[] speeds, int[] tempDefaults, int[] speedDefaults)
    {
        if (speeds.Length == 0) return (FanMode.Auto, 0);

        bool untouched = speeds.Length == speedDefaults.Length && temps.Length == tempDefaults.Length;
        for (int i = 0; untouched && i < speeds.Length; i++) untouched = speeds[i] == speedDefaults[i];
        for (int i = 0; untouched && i < temps.Length; i++) untouched = temps[i] == tempDefaults[i];
        if (untouched) return (FanMode.Auto, 0);

        foreach (var s in speeds)
            if (s != speeds[0]) return (FanMode.Curve, 0);
        return (FanMode.Fixed, speeds[0]);
    }

    // ------------------------------------------------------------------ setters

    public void SetCoreOffset(int gpuIndex, int offsetMhz)
    {
        var r = Range(gpuIndex, Od8Id.GfxClkFMax);
        if (!r.Supported) throw new GpuBackendException("This card does not expose a core clock offset.");
        Write(gpuIndex, (Od8Id.GfxClkFMax, Math.Clamp(offsetMhz, r.Min, r.Max)));
    }

    public void SetMemoryOffset(int gpuIndex, int mhz)
    {
        var r = Range(gpuIndex, Od8Id.UClkFMax);
        if (!r.Supported) throw new GpuBackendException("This card does not expose a memory clock control.");
        Write(gpuIndex, (Od8Id.UClkFMax, Math.Clamp(mhz, r.Min, r.Max)));
    }

    public void SetPowerLimit(int gpuIndex, int percent)
    {
        var r = Range(gpuIndex, Od8Id.PowerPercentage);
        if (!r.Supported) throw new GpuBackendException("This card does not expose a power limit.");
        Write(gpuIndex, (Od8Id.PowerPercentage, Math.Clamp(percent, r.Min, r.Max)));
    }

    public void SetTempLimit(int gpuIndex, int celsius)
    {
        var r = Range(gpuIndex, Od8Id.OperatingTempMax);
        if (!r.Supported) throw new GpuBackendException("This driver keeps the temperature limit to itself.");
        Write(gpuIndex, (Od8Id.OperatingTempMax, Math.Clamp(celsius, r.Min, r.Max)));
    }

    public void SetVoltageBoost(int gpuIndex, int percent)
    {
        if (percent != 0) throw new GpuBackendException("AMD cards have no voltage boost — use the voltage offset.");
    }

    /// <summary>Negative mV offset applied to the whole curve. 0 restores stock.</summary>
    public void SetVoltageCurveOffset(int gpuIndex, int offsetMv, int extraClockMhz = 0)
    {
        var r = Range(gpuIndex, Od8Id.OdVoltage);
        if (!r.Supported)
        {
            if (offsetMv == 0) return;
            throw new GpuBackendException("This card does not expose a voltage offset.");
        }
        Write(gpuIndex, (Od8Id.OdVoltage, Math.Clamp(offsetMv, r.Min, r.Max)));
    }

    public void SetZeroRpm(int gpuIndex, bool enabled)
    {
        if (!Range(gpuIndex, Od8Id.FanZeroRpmControl).Supported)
            throw new GpuBackendException("This card does not expose zero RPM control.");
        Write(gpuIndex, (Od8Id.FanZeroRpmControl, enabled ? 1 : 0));
    }

    public void SetMemoryTiming(int gpuIndex, int level)
    {
        var r = Range(gpuIndex, Od8Id.AcTiming);
        if (!r.Supported) throw new GpuBackendException("This card does not expose memory timing tuning.");
        Write(gpuIndex, (Od8Id.AcTiming, Math.Clamp(level, r.Min, r.Max)));
    }

    /// <summary>
    /// Fixed speed is expressed as a flat hardware curve: five points at the same duty. The driver
    /// still owns fan control, so it keeps working when this app is closed.
    /// </summary>
    public void SetFanSpeed(int gpuIndex, int fanIndex, int percent)
    {
        var s = Range(gpuIndex, Od8Id.FanCurveSpeed1);
        var t = Range(gpuIndex, Od8Id.FanCurveTemperature1);
        if (!s.Supported) throw new GpuBackendException("This card does not expose fan control.");

        int duty = Math.Clamp(percent, s.Min, s.Max);
        int lo = t.Min, hi = t.Max;
        Write(gpuIndex,
            (Od8Id.FanCurveTemperature1, lo), (Od8Id.FanCurveSpeed1, duty),
            (Od8Id.FanCurveTemperature2, lo + (hi - lo) / 4), (Od8Id.FanCurveSpeed2, duty),
            (Od8Id.FanCurveTemperature3, lo + (hi - lo) / 2), (Od8Id.FanCurveSpeed3, duty),
            (Od8Id.FanCurveTemperature4, lo + 3 * (hi - lo) / 4), (Od8Id.FanCurveSpeed4, duty),
            (Od8Id.FanCurveTemperature5, hi), (Od8Id.FanCurveSpeed5, duty));
    }

    /// <summary>Push a fan curve into the hardware, sampled down to the five points the driver takes.</summary>
    public void SetFanCurve(int gpuIndex, FanCurve curve)
    {
        var s = Range(gpuIndex, Od8Id.FanCurveSpeed1);
        var t = Range(gpuIndex, Od8Id.FanCurveTemperature1);
        if (!s.Supported) throw new GpuBackendException("This card does not expose fan control.");

        // Five evenly spaced temperatures across the allowed window, each taking its speed from the
        // user's curve. Temperatures must stay strictly increasing or the driver rejects the table.
        var points = new (Od8Id t, Od8Id s)[]
        {
            (Od8Id.FanCurveTemperature1, Od8Id.FanCurveSpeed1),
            (Od8Id.FanCurveTemperature2, Od8Id.FanCurveSpeed2),
            (Od8Id.FanCurveTemperature3, Od8Id.FanCurveSpeed3),
            (Od8Id.FanCurveTemperature4, Od8Id.FanCurveSpeed4),
            (Od8Id.FanCurveTemperature5, Od8Id.FanCurveSpeed5)
        };
        var writes = new List<(Od8Id, int)>(10);
        int lastTemp = int.MinValue, lastSpeed = s.Min;
        for (int i = 0; i < points.Length; i++)
        {
            int temp = t.Min + (t.Max - t.Min) * i / (points.Length - 1);
            if (temp <= lastTemp) temp = lastTemp + 1;
            int speed = (int)Math.Round(curve.Evaluate(temp));
            speed = Math.Clamp(speed, s.Min, s.Max);
            if (speed < lastSpeed) speed = lastSpeed;       // the hardware curve must be monotonic
            writes.Add((points[i].t, temp));
            writes.Add((points[i].s, speed));
            lastTemp = temp; lastSpeed = speed;
        }
        Write(gpuIndex, writes.ToArray());
    }

    public void SetFanAuto(int gpuIndex)
    {
        var ids = new[]
        {
            Od8Id.FanCurveTemperature1, Od8Id.FanCurveSpeed1,
            Od8Id.FanCurveTemperature2, Od8Id.FanCurveSpeed2,
            Od8Id.FanCurveTemperature3, Od8Id.FanCurveSpeed3,
            Od8Id.FanCurveTemperature4, Od8Id.FanCurveSpeed4,
            Od8Id.FanCurveTemperature5, Od8Id.FanCurveSpeed5
        };
        var writes = ids.Where(i => Range(gpuIndex, i).Supported)
                        .Select(i => (i, Range(gpuIndex, i).Default)).ToArray();
        if (writes.Length > 0) Write(gpuIndex, writes);
    }

    public void ResetToDefaults(int gpuIndex)
    {
        var errors = new List<string>();
        void Try(Action a) { try { a(); } catch (Exception e) { errors.Add(e.Message); } }

        var writes = new List<(Od8Id, int)>();
        foreach (Od8Id id in Enum.GetValues<Od8Id>())
        {
            var r = Range(gpuIndex, id);
            if (r.Supported) writes.Add((id, r.Default));
        }
        Try(() => { if (writes.Count > 0) Write(gpuIndex, writes.ToArray()); });
        Try(() => SetFanAuto(gpuIndex));

        if (errors.Count > 0)
            throw new GpuBackendException("Reset partially failed: " + string.Join(" | ", errors));
    }

    public string GetDiagnostics(int gpuIndex)
    {
        var sb = new System.Text.StringBuilder();
        var d = _devices[gpuIndex];
        sb.AppendLine($"GPU  : {d.Name}   ({d.BusId})");
        sb.AppendLine($"ADL adapter index: {Adapter(gpuIndex)}");
        sb.AppendLine();

        int sup = 0, en = 0, ver = 0;
        int rc = AdlNative.ADL2_Overdrive_Caps(_context, Adapter(gpuIndex), ref sup, ref en, ref ver);
        sb.AppendLine($"--- Overdrive ---");
        sb.AppendLine($"  Overdrive_Caps -> {AdlNative.Describe(rc)}  supported={sup} enabled={en} version={ver}");
        sb.AppendLine($"  capability mask = 0x{(_caps.TryGetValue(gpuIndex, out int cm) ? cm : 0):X8}");
        foreach (Od8Feature f in Enum.GetValues<Od8Feature>())
            if (Has(gpuIndex, f)) sb.AppendLine($"    {f}");
        sb.AppendLine();

        var cur = ReadCurrent(gpuIndex);
        sb.AppendLine("--- OD8 features (supported only) ---");
        foreach (Od8Id id in Enum.GetValues<Od8Id>())
        {
            var r = Range(gpuIndex, id);
            string mark = r.Supported ? "" : "   (locked)";
            sb.AppendLine($"  {id,-24} [{r.Min,7} .. {r.Max,7}]  default={r.Default,-7} current={cur[(int)id]}{mark}");
        }
        sb.AppendLine();

        sb.AppendLine("--- PMLog sensors ---");
        var t = ReadTelemetry(gpuIndex);
        sb.AppendLine($"  core {t.CoreClockMhz:0} MHz · mem {t.MemoryClockMhz:0} MHz · {t.VoltageMv:0} mV");
        sb.AppendLine($"  edge {t.TemperatureC:0}°C · hotspot {t.HotSpotC:0}°C · memory {t.MemoryTemperatureC:0}°C");
        sb.AppendLine($"  fan {t.FanPercent:0}% / {t.FanRpm:0} rpm · board {t.PowerWatts:0} W · load {t.GpuLoadPercent:0}%");
        sb.AppendLine($"  limit reason: {t.LimitReason}");
        return sb.ToString();
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_context == IntPtr.Zero) return;
            try { AdlNative.ADL2_Main_Control_Destroy(_context); } catch { }
            _context = IntPtr.Zero;
        }
    }

    /// <summary>
    /// Throws once the context is gone. Every call that reaches the driver checks this first, so a
    /// poll still in flight when Dispose runs fails cleanly instead of handing ADL a dead pointer.
    /// </summary>
    private IntPtr Live()
    {
        lock (_gate)
        {
            if (_context == IntPtr.Zero) throw new GpuBackendException("The AMD backend has been disposed.");
            return _context;
        }
    }
}
