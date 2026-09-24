using System.Runtime.InteropServices;
using System.Text;
using GpuTuner.Core.Models;
using static GpuTuner.Core.Backends.Intel.Igcl;

namespace GpuTuner.Core.Backends.Intel;

/// <summary>Intel Arc tuning through the public Graphics Control Library (IGCL).</summary>
public sealed class IntelBackend : IGpuBackend
{
    private Igcl? _api;
    private IntPtr _context;
    private readonly List<Card> _cards = new();
    private readonly List<GpuDevice> _devices = new();
    public string BackendName => "Intel IGCL";
    public IReadOnlyList<GpuDevice> Devices => _devices;
    private Igcl Api => _api ?? throw new GpuBackendException("Intel API is not initialized.");
    private sealed class Card
    {
        internal IntPtr Handle, CoreFrequency;
        internal int FrequencyMin, FrequencyMax;
        internal IntPtr[] Fans = [], Memory = [];
        internal (IntPtr Handle, int Type)[] Temperatures = [];
        internal Control[] Controls = [];
        internal GpuCapabilities Caps = new();
        internal GpuGraphicsInfo Graphics = new();
        internal double GpuEnergy = double.NaN, VramEnergy = double.NaN, RenderActivity = double.NaN, MediaActivity = double.NaN;
        internal double Time = double.NaN, Energy = double.NaN, Activity = double.NaN;
    }
    internal readonly record struct Control(bool Supported, int Units, double Min, double Max, double Step, double Default)
    {
        internal static Control Read(byte[] b, int index)
        {
            int at = 8 + index * 48;
            var c = new Control(b[at] != 0, Int(b, at + 4), BitConverter.ToDouble(b, at + 8),
                BitConverter.ToDouble(b, at + 16), BitConverter.ToDouble(b, at + 24), BitConverter.ToDouble(b, at + 32));
            return c with { Supported = c.Supported && double.IsFinite(c.Min) && double.IsFinite(c.Max)
                && c.Min <= c.Max && c.Step > 0 && double.IsFinite(c.Step) && double.IsFinite(c.Default) };
        }
        internal double Validate(double value)
        {
            if (!Supported || !double.IsFinite(value) || value < Min || value > Max)
                throw new GpuBackendException($"Intel value must be within the supported range {Min}–{Max}.");
            double snapped = Min + Math.Round((value - Min) / Step) * Step;
            if (Math.Abs(snapped - value) > 0.00001)
                throw new GpuBackendException($"Intel value must use steps of {Step} from {Min}.");
            return value;
        }
    }
    private byte[] Read(IntPtr h, string function, int size, byte version = 0)
    {
        var b = Data(size, version); Check(Api.Function<Igcl.Buffer>(function)(h, b), function); return b;
    }
    public void Initialize()
    {
        if (_cards.Count > 0) return;
        if (_api != null) throw new GpuBackendException("Intel API initialization previously failed.");
        _api = new Igcl();
        var init = Data(36); Put(init, 8, 0x10001); Put(init, 12, 3); // USE_LEVEL_ZERO | IGSC_FUL: permits read-only firmware identity queries.
        uint result = Api.Function<Init>("ctlInit")(init, out _context);
        if (result != 0)
        {
            // Older runtimes may not support firmware discovery. Keep tuning available.
            Put(init, 12, 1);
            Check(Api.Function<Init>("ctlInit")(init, out _context), "initialize");
        }
        foreach (var h in Api.Handles("ctlEnumerateDevices", _context))
        {
            var props = Data(320, 3);
            var luid = Marshal.AllocHGlobal(8);
            try
            {
                BitConverter.GetBytes(luid.ToInt64()).CopyTo(props, 8); Put(props, 16, 8);
                Check(Api.Function<Igcl.Buffer>("ctlGetDeviceProperties")(h, props), "device properties");
            }
            finally { Marshal.FreeHGlobal(luid); }
            if (Int(props, 20) != 1 || Int(props, 64) != 0x8086 || (Int(props, 188) & 1) != 0) continue;
            var c = new Card { Handle = h };
            var oc = Read(h, "ctlOverclockGetProperties", 440, 1);
            c.Controls = Enumerable.Range(0, 9).Select(i => Control.Read(oc, i)).ToArray();
            bool Has(int i, int units) => oc[5] != 0 && c.Controls[i].Supported && c.Controls[i].Units == units;
            try { c.Fans = Api.Handles("ctlEnumFans", h); } catch (GpuBackendException) { }
            try { c.Memory = Api.Handles("ctlEnumMemoryModules", h); } catch (GpuBackendException) { }
            try
            {
                c.Temperatures = Api.Handles("ctlEnumTemperatureSensors", h)
                    .Select(t => (t, Int(Read(t, "ctlTemperatureGetProperties", 24), 8))).ToArray();
            }
            catch (GpuBackendException) { }
            long vram = 0;
            var memoryProperties = new List<byte[]>();
            foreach (var m in c.Memory)
            {
                try
                {
                    var p = Read(m, "ctlMemoryGetProperties", 32);
                    if (Int(p, 12) != 1) continue; // Device-local memory only.
                    vram += (long)(BitConverter.ToUInt64(p, 16) / 1048576);
                    memoryProperties.Add(p);
                }
                catch (GpuBackendException) { }
            }
            bool fanControl = c.Fans.Length > 0; int fanPoints = 32;
            foreach (var fan in c.Fans)
            {
                var p = Read(fan, "ctlFanGetProperties", 24);
                // B60 32.0.101.9030 advertises RPM/fixed only, yet exposes and accepts
                // percent tables (verified on hardware). An existing percent table is
                // also evidence of support; no probing writes during initialization.
                var cfg = Read(fan, "ctlFanGetConfig", 936);
                int count = Int(cfg, 36);
                bool percentTable = Int(cfg, 8) == 2 && count is >= 2 and <= 32
                    && Enumerable.Range(0, count).All(j => Int(cfg, 64 + j * 28) == 1);
                bool b60Table = Int(props, 68) == 0xE211 && Int(p, 20) == 10;
                fanControl &= p[5] != 0 && (((Int(p, 8) & 4) != 0 && (Int(p, 12) & 2) != 0) || percentTable || b60Table);
                fanPoints = Math.Min(fanPoints, Int(p, 20));
            }
            bool curve = false;
            try { curve = Has(7, 13) && Has(8, 0) && Curve(h, 0).Length > 0 && Curve(h, 1).Length > 0; }
            catch (GpuBackendException) { }
            try
            {
                foreach (var frequency in Api.Handles("ctlEnumFrequencyDomains", h))
                {
                    var info = Read(frequency, "ctlFrequencyGetProperties", 32);
                    double lo = BitConverter.ToDouble(info, 16), hi = BitConverter.ToDouble(info, 24);
                    if (Int(info, 8) != 0 || info[12] == 0 || !double.IsFinite(lo) || !double.IsFinite(hi)
                        || lo <= 0 || hi < lo || hi > 100000) continue;
                    ReadFrequencyRange(frequency); // A usable control must also support readback.
                    c.CoreFrequency = frequency; c.FrequencyMin = (int)Math.Ceiling(lo); c.FrequencyMax = (int)Math.Floor(hi);
                    break;
                }
            }
            catch (GpuBackendException) { }
            c.Caps = new GpuCapabilities
            {
                CanLockClocks = c.CoreFrequency != IntPtr.Zero, ClockLockMinMhz = c.FrequencyMin, ClockLockMaxMhz = c.FrequencyMax,
                CanSetCoreOffset = Has(0, 0), CoreOffsetMinMhz = (int)c.Controls[0].Min, CoreOffsetMaxMhz = (int)c.Controls[0].Max,
                CoreOffsetStepMhz = Math.Max(1, (int)c.Controls[0].Step),
                CanSetMemoryOffset = Has(6, 12), MemoryClockIsAbsolute = true, MemoryClockUnit = "Mbps",
                MemoryOffsetMinMhz = (int)Math.Round(c.Controls[6].Min * 1000), MemoryOffsetMaxMhz = (int)Math.Round(c.Controls[6].Max * 1000),
                MemoryClockDefaultMhz = (int)Math.Round(c.Controls[6].Default * 1000),
                CanSetPowerLimit = Has(4, 11), PowerLimitMinPercent = (int)c.Controls[4].Min, PowerLimitMaxPercent = (int)c.Controls[4].Max,
                PowerLimitDefaultPercent = (int)c.Controls[4].Default,
                CanSetTempLimit = Has(5, 11), TempLimitIsPercent = true, TempLimitMinC = (int)c.Controls[5].Min,
                TempLimitMaxC = (int)c.Controls[5].Max, TempLimitDefaultC = (int)c.Controls[5].Default,
                VoltageStyle = VoltageControlStyle.Percent, CanSetVoltageBoost = Has(1, 11), CanReadVoltage = true,
                VoltageBoostMinPercent = (int)c.Controls[1].Min, VoltageBoostMaxPercent = (int)c.Controls[1].Max,
                VoltageBoostDefaultPercent = (int)c.Controls[1].Default,
                CanEditVfCurve = curve, VfCurveStepMhz = Math.Max(1, (int)c.Controls[8].Step),
                VfCurveMinMhz = (int)c.Controls[8].Min, VfCurveMaxMhz = (int)c.Controls[8].Max,
                MinVoltageMv = (int)c.Controls[7].Min, MaxVoltageMv = (int)c.Controls[7].Max,
                CanSetFanSpeed = fanControl && fanPoints >= 2, FanCurveIsHardware = true, CanReadHardwareFanCurve = true,
                FanCurvePoints = Math.Clamp(fanPoints, 0, 32), FanCount = c.Fans.Length,
                FanCurveMinTempC = 25, FanCurveMaxTempC = 100
            };
            ulong driver = BitConverter.ToUInt64(props, 32);
            string name = Encoding.UTF8.GetString(props, 88, 100).TrimEnd('\0');
            string version = $"{driver >> 48}.{(driver >> 32) & 0xffff}.{(driver >> 16) & 0xffff}.{driver & 0xffff}";
            string bios = "";
            try
            {
                foreach (var firmware in Api.Handles("ctlEnumerateFirmwareComponents", h))
                {
                    string candidate = OptionRomVersion(Read(firmware, "ctlGetFirmwareComponentProperties", 156));
                    if (candidate.Length > 0) { bios = candidate; break; }
                }
            }
            catch (Exception ex) when (ex is GpuBackendException or EntryPointNotFoundException) { }
            var device = new GpuDevice(_cards.Count, name, "Intel", "", version, vram, bios);
            c.Graphics = DecodeGraphics(device, props, memoryProperties);
            try
            {
                var pci = Data(64); Put(pci, 8, 24); Put(pci, 32, 24);
                Check(Api.Function<Igcl.Buffer>("ctlPciGetProperties")(h, pci), "PCIe properties");
                var identity = DecodePci(pci);
                device = device with { BusId = identity.Address };
                c.Graphics = c.Graphics with { BusAddress = identity.Address, BusInterface = identity.Interface, ResizableBar = identity.Rebar };
            }
            catch (GpuBackendException) { }
            _devices.Add(device);
            _cards.Add(c);
        }
        if (_cards.Count == 0) throw new GpuBackendException("No discrete Intel Arc GPU found.");
    }
    // Offsets follow the x64 IGCL structures, including nested Size/Version headers.
    internal static (string Interface, string Address, string Rebar) DecodePci(byte[] pci) => (
        GpuIdentity.PcieBusInterface(Int(pci, 40), Int(pci, 44), 0, 0),
        $"{Int(pci, 16):X4}:{Int(pci, 20):X2}:{Int(pci, 24):X2}.{Int(pci, 28)}",
        pci[56] == 0 ? "Not supported" : pci[57] != 0 ? "Enabled" : "Disabled");

    internal static string OptionRomVersion(byte[] firmware)
    {
        string name = Encoding.UTF8.GetString(firmware, 5, 64).Split('\0')[0].Trim();
        string version = Encoding.UTF8.GetString(firmware, 69, 64).Split('\0')[0].Trim();
        // GSC firmware and OptionRomData are not the VBIOS code version.
        return name.Equals("OptionRomCode", StringComparison.OrdinalIgnoreCase)
            && version.Length > 0 && !version.Equals("unknown", StringComparison.OrdinalIgnoreCase)
            && !version.Equals("Not Implemented", StringComparison.OrdinalIgnoreCase) ? version : "";
    }
    internal static GpuGraphicsInfo DecodeGraphics(GpuDevice device, byte[] props, IReadOnlyList<byte[]> memory)
    {
        // Public IGCL device v3 / memory properties ABI. Do not infer memory vendor or ROP counts.
        bool b60 = Int(props, 68) == 0xE211;
        uint cores = BitConverter.ToUInt32(props, 204);
        string MemoryType(int type) => type switch
        {
            0 => "HBM", 1 => "DDR", 2 => "DDR3", 3 => "DDR4", 4 => "DDR5",
            5 => "LPDDR", 6 => "LPDDR3", 7 => "LPDDR4", 8 => "LPDDR5",
            9 => "GDDR4", 10 => "GDDR5", 11 => "GDDR5X", 12 => "GDDR6",
            13 => "GDDR6X", 14 => "GDDR7", _ => ""
        };
        return GpuGraphicsInfo.FromDevice(device) with
        {
            BoardManufacturer = GpuIdentity.BoardVendor(BitConverter.ToUInt16(props, 198)),
            Revision = $"{Int(props, 72):X2}",
            Cores = cores > 0 ? $"{cores} Xe cores" : "",
            CodeName = b60 ? "Battlemage BMG-G21 WKSTN" : "",
            // Intel B60 specifications: intel.com/content/www/us/en/products/sku/243916/
            Technology = b60 ? "5 nm (TSMC N5)" : "",
            MemoryType = string.Join(" / ", memory.Select(p => MemoryType(Int(p, 8))).Where(t => t.Length > 0).Distinct()),
            // Avoid inventing an aggregate width when a driver exposes multiple modules.
            BusWidth = memory.Count == 1 && Int(memory[0], 24) > 0 ? $"{Int(memory[0], 24)} bit" : "",
            ResizableBar = "Unavailable"
        };
    }

    public GpuGraphicsInfo ReadGraphicsInfo(int gpuIndex) => _cards[gpuIndex].Graphics;
    public GpuCapabilities GetCapabilities(int gpuIndex) => _cards[gpuIndex].Caps;
    private double Get(Card c, string name)
    {
        Check(Api.Function<GetDouble>("ctlOverclock" + name + "GetV2")(c.Handle, out double value), name + " read");
        if (!double.IsFinite(value)) throw new GpuBackendException("Intel returned a non-finite tuning value.");
        return value;
    }
    private void Waiver(Card c) => Check(Api.Function<Handle>("ctlOverclockWaiverSet")(c.Handle), "enable tuning");
    private void Set(int index, int control, string name, double value)
    {
        var c = _cards[index]; c.Controls[control].Validate(value);
        if (Math.Abs(Get(c, name) - value) < 0.00001) return;
        Waiver(c);
        Check(Api.Function<SetDouble>("ctlOverclock" + name + "SetV2")(c.Handle, value), name + " write");
        double actual = Get(c, name);
        if (Math.Abs(actual - value) > 0.00001)
            throw new GpuBackendException($"Intel {name} readback mismatch: requested {value}, got {actual}.");
    }
    private (double Min, double Max) ReadFrequencyRange(IntPtr handle)
    {
        var data = Read(handle, "ctlFrequencyGetRange", 24);
        double lo = BitConverter.ToDouble(data, 8), hi = BitConverter.ToDouble(data, 16);
        if (!double.IsFinite(lo) || !double.IsFinite(hi) || lo < 0 || hi < lo)
            throw new GpuBackendException("Intel did not report a usable core clock range.");
        return (lo, hi);
    }
    internal static byte[] ClockRangeRequest(int minMhz, int maxMhz, int supportedMin, int supportedMax)
    {
        bool reset = minMhz == 0 && maxMhz == 0;
        int lo = minMhz == 0 ? supportedMin : minMhz, hi = maxMhz == 0 ? supportedMax : maxMhz;
        if (!reset && (lo < supportedMin || hi > supportedMax || lo > hi))
            throw new GpuBackendException($"Core clock range must be ordered and within {supportedMin}–{supportedMax} MHz.");
        var data = Data(24);
        BitConverter.GetBytes(reset ? -1.0 : lo).CopyTo(data, 8);
        BitConverter.GetBytes(reset ? -1.0 : hi).CopyTo(data, 16);
        return data;
    }
    public void SetClockRange(int i, int minMhz, int maxMhz)
    {
        var c = _cards[i];
        if (!c.Caps.CanLockClocks) throw new GpuBackendException("Intel core clock range control is unavailable.");
        var request = ClockRangeRequest(minMhz, maxMhz, c.FrequencyMin, c.FrequencyMax);
        var before = ReadFrequencyRange(c.CoreFrequency);
        double lo = BitConverter.ToDouble(request, 8), hi = BitConverter.ToDouble(request, 16);
        if (Math.Abs(before.Min - lo) < .5 && Math.Abs(before.Max - hi) < .5) return;
        try
        {
            Check(Api.Function<Igcl.Buffer>("ctlFrequencySetRange")(c.CoreFrequency, request), "core clock range");
            var actual = ReadFrequencyRange(c.CoreFrequency);
            if (lo >= 0 && (Math.Abs(actual.Min - lo) > .5 || Math.Abs(actual.Max - hi) > .5))
                throw new GpuBackendException($"Intel clock range readback mismatch: requested {lo}–{hi}, got {actual.Min}–{actual.Max} MHz.");
        }
        catch (Exception ex) when (ex is GpuBackendException)
        {
            var restore = Data(24); BitConverter.GetBytes(before.Min).CopyTo(restore, 8); BitConverter.GetBytes(before.Max).CopyTo(restore, 16);
            uint status = Api.Function<Igcl.Buffer>("ctlFrequencySetRange")(c.CoreFrequency, restore);
            if (status != 0) throw new GpuBackendException(ex.Message + $" Restoring the previous clock range also failed (0x{status:X8}).", ex);
            var restored = ReadFrequencyRange(c.CoreFrequency);
            if (Math.Abs(restored.Min - before.Min) > .5 || Math.Abs(restored.Max - before.Max) > .5)
                throw new GpuBackendException(ex.Message + " Previous clock range could not be verified after restore.", ex);
            throw;
        }
    }
    public void SetCoreOffset(int i, int value) => Set(i, 0, "GpuFrequencyOffset", value);
    public void SetMemoryOffset(int i, int value) => Set(i, 6, "VramMemSpeedLimit", value / 1000.0);
    public void SetVoltageBoost(int i, int value) => Set(i, 1, "GpuMaxVoltageOffset", value);
    public void SetPowerLimit(int i, int value) => Set(i, 4, "PowerLimit", value);
    public void SetTempLimit(int i, int value) => Set(i, 5, "TemperatureLimit", value);
    public void SetVoltageCurveOffset(int i, int offsetMv, int extraClockMhz = 0) =>
        throw new GpuBackendException("Use Intel's V/F curve editor or voltage limit percentage.");
    public int ReadVoltageLockMv(int i) => 0;
    public GpuTuningState ReadTuningState(int i)
    {
        var c = _cards[i];
        int Value(int control, string name, double scale = 1) => c.Controls[control].Supported ? (int)Math.Round(Get(c, name) * scale) : 0;
        FanMode mode = FanMode.Auto; int percent = 0; FanCurve? fanCurve = null;
        if (c.Fans.Length > 0)
        {
            var cfg = Read(c.Fans[0], "ctlFanGetConfig", 936);
            mode = Int(cfg, 8) switch { 1 => FanMode.Fixed, 2 => FanMode.Curve, _ => FanMode.Auto };
            if (mode == FanMode.Fixed && Int(cfg, 24) == 1) percent = Int(cfg, 20);
            if (mode == FanMode.Curve)
            {
                int count = Int(cfg, 36);
                if (count is > 0 and <= 32)
                {
                    var points = new List<FanPoint>();
                    for (int j = 0; j < count; j++)
                    {
                        int at = 40 + 28 * j;
                        if (Int(cfg, at + 24) != 1) { points.Clear(); break; }
                        points.Add(new FanPoint(Int(cfg, at + 8), Int(cfg, at + 20)));
                    }
                    if (points.Count > 0)
                    {
                        fanCurve = new FanCurve { Points = points };
                        if (points.All(p => p.FanPercent == points[0].FanPercent))
                        { mode = FanMode.Fixed; percent = (int)points[0].FanPercent; }
                    }
                }
            }
        }
        var clockRange = c.Caps.CanLockClocks ? ReadFrequencyRange(c.CoreFrequency) : (Min: 0.0, Max: 0.0);
        return new GpuTuningState
        {
            LockedClockMinMhz = (int)Math.Round(clockRange.Min), LockedClockMaxMhz = (int)Math.Round(clockRange.Max),
            CoreOffsetMhz = Value(0, "GpuFrequencyOffset"), MemoryOffsetMhz = Value(6, "VramMemSpeedLimit", 1000),
            VoltageBoostPercent = Value(1, "GpuMaxVoltageOffset"), PowerLimitPercent = Value(4, "PowerLimit"),
            TempLimitC = Value(5, "TemperatureLimit"), FanManual = mode == FanMode.Fixed, FanPercent = percent,
            DetectedFanMode = mode, HardwareFanCurve = fanCurve
        };
    }
    private Point[] Curve(IntPtr h, int type)
    {
        var f = Api.Function<ReadCurve>("ctlOverclockReadVFCurve"); uint count = 0;
        Check(f(h, type, 0, ref count, null), "V/F curve count");
        if (count == 0) return [];
        if (count > 1024) throw new GpuBackendException("Intel returned an invalid curve size.");
        var p = new Point[count]; Check(f(h, type, 0, ref count, p), "read V/F curve");
        if (count != p.Length) throw new GpuBackendException("Intel V/F curve changed while reading.");
        return p;
    }
    public IReadOnlyList<VfCurveSample> ReadVfCurve(int i)
    {
        var c = _cards[i]; var stock = Curve(c.Handle, 0); var live = Curve(c.Handle, 1);
        if (stock.Length != live.Length)
            throw new GpuBackendException("Intel stock and live curve counts differ; reload the curve after the driver settles.");
        return stock.Select((p, j) => new VfCurveSample(j, (int)live[j].Voltage, (int)p.Frequency, (int)live[j].Frequency)).ToArray();
    }
    public void SetVfCurveTargets(int i, IReadOnlyList<VfCurveSample> targets)
    {
        var c = _cards[i]; if (!c.Caps.CanEditVfCurve) throw new GpuBackendException("Intel V/F editing is unavailable.");
        var points = Curve(c.Handle, 0);
        var seen = new HashSet<int>();
        foreach (var t in targets)
        {
            if (t.Index < 0 || t.Index >= points.Length || !seen.Add(t.Index))
                throw new GpuBackendException("Invalid Intel curve point index or voltage.");
            // IGCL's voltage coordinates move with operating conditions. This editor
            // changes frequencies by index, so use freshly read voltage coordinates.
            c.Controls[8].Validate(t.LiveMhz); points[t.Index].Frequency = (uint)t.LiveMhz;
        }
        var stock = Curve(c.Handle, 0);
        if (points.Length == stock.Length && points.Select(p => p.Frequency).SequenceEqual(stock.Select(p => p.Frequency)))
        {
            // A zero global offset clears a custom table without re-interpolating a
            // stale voltage grid. Writing stock points directly can shift frequencies.
            Waiver(c);
            Check(Api.Function<SetDouble>("ctlOverclockGpuFrequencyOffsetSetV2")(c.Handle, 0), "clear custom V/F curve");
            for (int attempt = 0; attempt < 10; attempt++)
            {
                Thread.Sleep(50);
                if (Curve(c.Handle, 1).Select(p => p.Frequency).SequenceEqual(stock.Select(p => p.Frequency))) return;
            }
            throw new GpuBackendException("Intel could not confirm that the custom curve was cleared.");
        }
        for (int j = 1; j < points.Length; j++)
            if (points[j].Voltage <= points[j - 1].Voltage || points[j].Frequency < points[j - 1].Frequency)
                throw new GpuBackendException("Intel curve frequencies must not decrease as voltage increases.");
        var before = Curve(c.Handle, 1);
        if (points.SequenceEqual(before)) return;
        Waiver(c);
        Check(Api.Function<WriteCurve>("ctlOverclockWriteCustomVFCurve")(c.Handle, (uint)points.Length, points), "write V/F curve");
        // IGCL accepts the request before firmware publishes the new curve. Immediate
        // reads can return the old curve or a transient point count. Bound the wait.
        for (int attempt = 0; attempt < 10; attempt++)
        {
            Thread.Sleep(50);
            try { if (points.Select(p => p.Frequency).SequenceEqual(Curve(c.Handle, 1).Select(p => p.Frequency))) return; }
            catch (GpuBackendException) when (attempt < 9) { }
        }
        throw new GpuBackendException("Intel adjusted or did not apply the requested V/F curve. Reload to see the actual points.");
    }
    public void SetFanCurve(int i, FanCurve curve)
    {
        var c = _cards[i];
        if (!c.Caps.CanSetFanSpeed) throw new GpuBackendException("This Intel driver does not advertise percentage fan-curve writes.");
        var table = BuildFanTable(curve, c.Caps.FanCurvePoints);
        int pointCount = Int(table, 8);
        Waiver(c);
        foreach (var fan in c.Fans)
        {
            var pinned = GCHandle.Alloc(table, GCHandleType.Pinned);
            try
            {
                Check(Api.Function<PointerBuffer>("ctlFanSetSpeedTableMode")(fan, pinned.AddrOfPinnedObject()), "fan curve write");
                bool verified = false;
                for (int attempt = 0; attempt < 10 && !verified; attempt++)
                {
                    Thread.Sleep(50);
                    var cfg = Read(fan, "ctlFanGetConfig", 936);
                    verified = Int(cfg, 8) == 2 && Int(cfg, 36) == pointCount
                        && Enumerable.Range(0, pointCount).All(j => Int(cfg, 48 + j * 28) == Int(table, 20 + j * 28)
                            && Int(cfg, 60 + j * 28) == Int(table, 32 + j * 28) && Int(cfg, 64 + j * 28) == 1);
                }
                if (!verified) throw new GpuBackendException("Intel fan curve readback mismatch. Reload to see the actual curve.");
            }
            finally { pinned.Free(); }
        }
    }
    internal static byte[] BuildFanTable(FanCurve curve, int maxPoints)
    {
        var points = curve.Points.OrderBy(p => p.TemperatureC).ToArray();
        if (points.Length < 2 || points.Length > Math.Min(maxPoints, 32) || points.Any(p => !double.IsFinite(p.TemperatureC)
            || !double.IsFinite(p.FanPercent) || p.TemperatureC < 25 || p.TemperatureC > 100 || p.FanPercent < 0 || p.FanPercent > 100)
            || points.Zip(points.Skip(1), (a,b) => Math.Round(a.TemperatureC) >= Math.Round(b.TemperatureC) || a.FanPercent > b.FanPercent).Any(x => x))
            throw new GpuBackendException("Intel fan curves require increasing temperatures (25–100°C) and non-decreasing speeds (0–100%).");
        var table = Data(908); Put(table, 8, points.Length);
        for (int j = 0; j < points.Length; j++)
        {
            // Preserve the zero nested headers used in the driver's own fan table.
            int at = 12 + j * 28; Put(table, at + 8, (int)Math.Round(points[j].TemperatureC));
            Put(table, at + 20, (int)Math.Round(points[j].FanPercent)); Put(table, at + 24, 1);
        }
        return table;
    }
    public void SetFanSpeed(int i, int fanIndex, int percent)
    {
        if (fanIndex > 0) throw new GpuBackendException("Intel fan curve controls all fans together.");
        SetFanCurve(i, new FanCurve { Points = [new(25, percent), new(100, percent)] });
    }
    public void SetFanAuto(int i)
    {
        var c = _cards[i];
        if (!c.Caps.CanSetFanSpeed) throw new GpuBackendException("Intel fan control is unavailable through this driver.");
        foreach (var f in c.Fans)
        {
            Check(Api.Function<Handle>("ctlFanSetDefaultMode")(f), "restore automatic fans");
            bool verified = false;
            for (int attempt = 0; attempt < 10 && !verified; attempt++)
            {
                Thread.Sleep(50);
                verified = Int(Read(f, "ctlFanGetConfig", 936), 8) == 0;
            }
            if (!verified) throw new GpuBackendException("Intel automatic fan mode did not pass readback verification.");
        }
    }
    public void ResetToDefaults(int i)
    {
        var c = _cards[i]; Waiver(c);
        Check(Api.Function<Handle>("ctlOverclockResetToDefault")(c.Handle), "restore tuning defaults");
        if (c.Caps.CanLockClocks) SetClockRange(i, 0, 0);
        if (c.Caps.CanSetFanSpeed) SetFanAuto(i);
    }
    internal static double Rate(double current, double previous, double elapsed) =>
        double.IsFinite(current) && double.IsFinite(previous) && double.IsFinite(elapsed) && elapsed > 0 && current >= previous ? (current - previous) / elapsed : double.NaN;
    public GpuTelemetry ReadTelemetry(int i)
    {
        var c = _cards[i]; var b = Read(c.Handle, "ctlPowerTelemetryGetV2", 1016, 1);
        double time = Sensor(b, 8), energy = Sensor(b, 376), activity = Sensor(b, 128), dt = time - c.Time;
        double power = Rate(energy, c.Energy, dt), load = Rate(activity, c.Activity, dt) * 100;
        c.Time = time; c.Energy = energy; c.Activity = activity;
        var fans = Enumerable.Range(0, 5).Select(j => Sensor(b, 680 + j * 24)).Where(double.IsFinite).ToArray();
        double used = 0, available = 0; bool memoryRead = false;
        foreach (var m in c.Memory)
        {
            try { var state = Read(m, "ctlMemoryGetState", 24); ulong free = BitConverter.ToUInt64(state, 8), total = BitConverter.ToUInt64(state, 16);
                if (total >= free) { used += (total - free) / 1048576.0; available += free / 1048576.0; memoryRead = true; } }
            catch (GpuBackendException) { }
        }
        var extras = new List<SupplementalSensor>();
        void Extra(int offset, string key, string name, string unit) { double v = Sensor(b, offset); if (double.IsFinite(v)) extras.Add(new(key, name, unit, v)); }
        void Counter(int offset, ref double previous, string key, string name, string unit, double scale = 1)
        {
            double current = Sensor(b, offset), value = Rate(current, previous, dt) * scale;
            previous = current;
            if (double.IsFinite(current)) extras.Add(new(key, name, unit, value));
        }
        Counter(32, ref c.GpuEnergy, "intel-gpu-power", "GPU power", "W");
        Counter(208, ref c.VramEnergy, "intel-vram-power", "Memory power", "W");
        Counter(152, ref c.RenderActivity, "intel-render-load", "Render / compute", "%", 100);
        Counter(176, ref c.MediaActivity, "intel-media-load", "Media engine", "%", 100);
        Extra(848, "intel-sa-vr", "SA voltage regulator", "°C");
        Extra(232, "intel-memory-voltage", "Memory voltage", "V");
        if (memoryRead)
        {
            extras.Add(new("intel-memory-free", "Available VRAM", "MB", available));
            if (used + available > 0) extras.Add(new("intel-memory-usage", "VRAM usage", "%", used / (used + available) * 100));
        }
        foreach (var (offset, key, name) in new[] { (200, "power", "Power"), (201, "thermal", "Thermal"), (202, "current", "Current"), (203, "voltage", "Voltage"), (204, "utilization", "Utilization") })
            extras.Add(new("intel-limit-" + key, name + " limit active", "state", b[offset] != 0 ? 1 : 0));
        foreach (var (handle, type) in c.Temperatures)
        {
            string? name = type switch { 0 => "GPU global", 1 => "GPU core", _ => null };
            if (name == null) continue;
            try
            {
                Check(Api.Function<GetDouble>("ctlTemperatureGetState")(handle, out double temperature), "temperature");
                if (double.IsFinite(temperature)) extras.Add(new($"intel-temperature-{type}", name, "°C", temperature));
            }
            catch (GpuBackendException) { }
        }
        try
        {
            var pci = Data(32); Put(pci, 8, 24);
            Check(Api.Function<Igcl.Buffer>("ctlPciGetState")(c.Handle, pci), "PCIe state");
            int generation = Int(pci, 16), width = Int(pci, 20);
            double speed = generation switch { 1 => 2.5, 2 => 5, 3 => 8, 4 => 16, 5 => 32, 6 => 64, _ => double.NaN };
            if (double.IsFinite(speed)) extras.Add(new("intel-pcie-speed", "PCIe link speed", "GT/s", speed));
            if (width > 0) extras.Add(new("intel-pcie-width", "PCIe link width", "lanes", width));
        }
        catch (GpuBackendException) { }
        Extra(872, "intel-effective-clock", "GPU core (effective)", "MHz");
        Extra(800, "intel-gpu-vr", "GPU voltage regulator", "°C"); Extra(824, "intel-vram-vr", "Memory voltage regulator", "°C");
        Extra(968, "intel-memory-read", "Memory read bandwidth", "MB/s"); Extra(992, "intel-memory-write", "Memory write bandwidth", "MB/s");
        return new GpuTelemetry
        {
            CoreClockMhz = Sensor(b, 80), MemoryClockMhz = Sensor(b, 256), TemperatureC = Sensor(b, 104),
            VoltageMv = Sensor(b, 56) * 1000, HotSpotC = double.NaN, MemoryTemperatureC = Sensor(b, 352),
            PowerWatts = power, PowerPercent = Sensor(b, 920), GpuLoadPercent = double.IsFinite(load) ? Math.Clamp(load, 0, 100) : double.NaN,
            MemoryLoadPercent = double.NaN, MemoryUsedMb = memoryRead ? used : double.NaN, FanRpms = fans,
            FanRpm = fans.FirstOrDefault(double.NaN), FanPercent = double.NaN, SupplementalSensors = extras,
            LimitReason = string.Join(" / ", new[] { b[200] != 0 ? "Power" : "", b[201] != 0 ? "Thermal" : "", b[202] != 0 ? "Current" : "", b[203] != 0 ? "Voltage" : "", b[204] != 0 ? "Utilization" : "" }.Where(s => s.Length > 0))
        };
    }
    public string GetDiagnostics(int i) => System.Text.Json.JsonSerializer.Serialize(new { Device = Devices[i], Capabilities = GetCapabilities(i), State = ReadTuningState(i), Curve = ReadVfCurve(i) }, new System.Text.Json.JsonSerializerOptions { WriteIndented = true });
    public void Dispose()
    {
        if (_api == null) return;
        if (_context != IntPtr.Zero) { Api.Function<Handle>("ctlClose")(_context); _context = IntPtr.Zero; }
        _api.Dispose(); _api = null; _cards.Clear(); _devices.Clear();
    }
}
