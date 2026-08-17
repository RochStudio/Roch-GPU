using System.Runtime.InteropServices;
using NvAPIWrapper.Native.GPU.Structures;

namespace GpuTuner.Core.Backends.Nvidia;

/// <summary>
/// The one private NVAPI entry point NvAPIWrapper doesn't cover that we need:
/// NvAPI_GPU_GetThermalSensors (0x65FE3AAD) — hot-spot and memory-junction temperatures.
/// Same call and slot layout LibreHardwareMonitor / HWiNFO use.
/// </summary>
internal static class NvApiPrivate
{
    [StructLayout(LayoutKind.Sequential, Pack = 8)]
    private struct NvThermalSensorsV2
    {
        public uint Version;
        public uint Mask;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 8)] public int[] Reserved;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 32)] public int[] Temperatures;
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int GetThermalSensorsDelegate(IntPtr gpuHandle, ref NvThermalSensorsV2 sensors);

    [DllImport("nvapi64", EntryPoint = "nvapi_QueryInterface", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr QueryInterface64(uint id);

    private static GetThermalSensorsDelegate? _getThermalSensors;
    private static bool _resolved;

    private static GetThermalSensorsDelegate? Resolve()
    {
        if (_resolved) return _getThermalSensors;
        _resolved = true;
        try
        {
            var ptr = QueryInterface64(0x65FE3AAD);
            if (ptr != IntPtr.Zero)
                _getThermalSensors = Marshal.GetDelegateForFunctionPointer<GetThermalSensorsDelegate>(ptr);
        }
        catch { _getThermalSensors = null; }
        return _getThermalSensors;
    }

    /// <summary>
    /// Probe once to find the widest sensor mask the driver accepts (LHM's trick: grow bit by bit until it errors).
    /// Returns 0 if the call is unavailable.
    /// </summary>
    public static uint ProbeMask(PhysicalGPUHandle handle)
    {
        var fn = Resolve();
        if (fn == null) return 0;
        uint mask = 0;
        for (int bit = 0; bit < 32; bit++)
        {
            uint tryMask = 1u << bit;
            var s = new NvThermalSensorsV2
            {
                Version = (uint)Marshal.SizeOf<NvThermalSensorsV2>() | (2u << 16),
                Mask = tryMask,
                Reserved = new int[8],
                Temperatures = new int[32]
            };
            if (fn(handle.MemoryAddress, ref s) != 0) break;
            mask = tryMask;
        }
        // LHM uses (highest accepted bit << 1) - 1 i.e. all bits up to the highest one.
        return mask == 0 ? 0 : (mask << 1) - 1;
    }

    /// <summary>Raw temperature slots (°C, already divided by 256). Empty array on failure.</summary>
    public static double[] Read(PhysicalGPUHandle handle, uint mask)
    {
        var fn = Resolve();
        if (fn == null || mask == 0) return Array.Empty<double>();
        var s = new NvThermalSensorsV2
        {
            Version = (uint)Marshal.SizeOf<NvThermalSensorsV2>() | (2u << 16),
            Mask = mask,
            Reserved = new int[8],
            Temperatures = new int[32]
        };
        try
        {
            if (fn(handle.MemoryAddress, ref s) != 0) return Array.Empty<double>();
        }
        catch { return Array.Empty<double>(); }
        var r = new double[32];
        for (int i = 0; i < 32; i++) r[i] = s.Temperatures[i] / 256.0;
        return r;
    }

    // ------------------------------------------------------------------ V/F curve
    //
    // NvAPIWrapper builds these structs zeroed and calls straight through, which leaves the
    // *mask* field at 0. The mask selects which of the 103 curve points the driver should fill,
    // so with mask 0 the call returns Status.Ok and an entirely empty curve — exactly what a
    // 4070 Ti on driver 591.86 does. Same trap as GetThermalSensors above. So we re-declare the
    // structs here and set the mask ourselves.
    //
    // Layouts mirror NvAPIWrapper's PrivateVFPCurveV1 / PrivateClockBoostTableV1 exactly, with the
    // per-entry structs flattened to uint[] (entry stride 7 for the curve, 9 for the delta table).

    private const int CurveEntries = 80, MemCurveEntries = 23, CurveStride = 7;
    private const int DeltaEntries = 80, DeltaStride = 9;

    [StructLayout(LayoutKind.Sequential, Pack = 8)]
    private struct VfpCurveV1
    {
        public uint Version;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 4)] public uint[] Masks;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 12)] public uint[] Unknown1;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = CurveEntries * CurveStride)] public uint[] GpuEntries;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = MemCurveEntries * CurveStride)] public uint[] MemEntries;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 1064)] public uint[] Unknown2;

        public static VfpCurveV1 Create(uint[] mask) => new()
        {
            Version = (uint)Marshal.SizeOf<VfpCurveV1>() | (1u << 16),
            Masks = (uint[])mask.Clone(),
            Unknown1 = new uint[12],
            GpuEntries = new uint[CurveEntries * CurveStride],
            MemEntries = new uint[MemCurveEntries * CurveStride],
            Unknown2 = new uint[1064]
        };
    }

    [StructLayout(LayoutKind.Sequential, Pack = 8)]
    private struct BoostTableV1
    {
        public uint Version;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 4)] public uint[] Masks;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 12)] public uint[] Unknown1;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = DeltaEntries * DeltaStride)] public int[] GpuDeltas;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 23)] public uint[] MemoryFilled;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 23)] public int[] MemoryDeltas;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 1529)] public uint[] Unknown2;

        public static BoostTableV1 Create(uint[] mask) => new()
        {
            Version = (uint)Marshal.SizeOf<BoostTableV1>() | (1u << 16),
            Masks = (uint[])mask.Clone(),
            Unknown1 = new uint[12],
            GpuDeltas = new int[DeltaEntries * DeltaStride],
            MemoryFilled = new uint[23],
            MemoryDeltas = new int[23],
            Unknown2 = new uint[1529]
        };
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int VfpCurveDelegate(IntPtr gpu, ref VfpCurveV1 curve);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int BoostTableDelegate(IntPtr gpu, ref BoostTableV1 table);

    private static VfpCurveDelegate? _getVfpCurve;
    private static BoostTableDelegate? _getBoostTable, _setBoostTable;
    private static bool _curveResolved;

    private static void ResolveCurve()
    {
        if (_curveResolved) return;
        _curveResolved = true;
        try
        {
            var a = QueryInterface64(0x21537AD4);   // NvAPI_GPU_GetVFPCurve
            var b = QueryInterface64(0x23F1B133);   // NvAPI_GPU_GetClockBoostTable
            var c = QueryInterface64(0x0733E009);   // NvAPI_GPU_SetClockBoostTable
            if (a != IntPtr.Zero) _getVfpCurve = Marshal.GetDelegateForFunctionPointer<VfpCurveDelegate>(a);
            if (b != IntPtr.Zero) _getBoostTable = Marshal.GetDelegateForFunctionPointer<BoostTableDelegate>(b);
            if (c != IntPtr.Zero) _setBoostTable = Marshal.GetDelegateForFunctionPointer<BoostTableDelegate>(c);
        }
        catch { _getVfpCurve = null; _getBoostTable = null; _setBoostTable = null; }
    }

    /// <summary>All 128 bits set. Fine for reads; some drivers reject it on writes.</summary>
    public static uint[] FullMask => new[] { 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu };

    /// <summary>Exactly the 103 bits that correspond to real curve points, nothing above.</summary>
    public static uint[] Mask103 => new[] { 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu, 0x0000007Fu };

    /// <summary>Only the bits for points we're actually changing.</summary>
    public static uint[] MaskFor(int[] deltasKhz)
    {
        var m = new uint[4];
        for (int i = 0; i < deltasKhz.Length && i < 128; i++)
            if (deltasKhz[i] != 0) m[i / 32] |= 1u << (i % 32);
        return m;
    }

    /// <summary>Read the V/F curve with an explicit mask. Returns (voltageUv, frequencyKhz) per slot, or empty.</summary>
    public static (uint voltUv, int freqKhz)[] ReadCurve(PhysicalGPUHandle handle, uint[] mask)
    {
        ResolveCurve();
        if (_getVfpCurve == null) return Array.Empty<(uint, int)>();
        var s = VfpCurveV1.Create(mask);
        try { if (_getVfpCurve(handle.MemoryAddress, ref s) != 0) return Array.Empty<(uint, int)>(); }
        catch { return Array.Empty<(uint, int)>(); }

        // Read straight through both regions, exactly as ReadCurveRaw does. The curve does not stop
        // at the "GPU" array — on a 4070 Ti it is 103 points and spills into the 23-entry array
        // NvAPIWrapper labels "memory". Stopping at 80 truncated this card's curve at 945 mV instead
        // of 1090, and anything computed from it (an undervolt target, the stock ceiling) came out
        // measured against the wrong top. See TotalPoints.
        var r = new (uint, int)[TotalPoints(CurveEntries)];
        for (int i = 0; i < CurveEntries; i++)
            r[i] = (s.GpuEntries[i * CurveStride + 2], (int)s.GpuEntries[i * CurveStride + 1]);
        for (int i = 0; i < MemCurveEntries; i++)
            r[CurveEntries + i] = (s.MemEntries[i * CurveStride + 2], (int)s.MemEntries[i * CurveStride + 1]);
        return r;
    }

    /// <summary>Read the per-point frequency deltas (kHz) with an explicit mask.</summary>
    public static int[] ReadDeltas(PhysicalGPUHandle handle, uint[] mask)
    {
        ResolveCurve();
        if (_getBoostTable == null) return Array.Empty<int>();
        var s = BoostTableV1.Create(mask);
        try { if (_getBoostTable(handle.MemoryAddress, ref s) != 0) return Array.Empty<int>(); }
        catch { return Array.Empty<int>(); }

        // Same span as the curve: the points past the GPU array take their delta from one of the two
        // trailing 23-int arrays, picked by TrailingArray — the offsets here mirror DeltaOffset().
        var r = new int[TotalPoints(DeltaEntries)];
        for (int i = 0; i < DeltaEntries; i++) r[i] = s.GpuDeltas[i * DeltaStride + 5];
        for (int i = 0; i < MemCurveEntries; i++)
            r[DeltaEntries + i] = TrailingArray == 1 ? s.MemoryDeltas[i] : unchecked((int)s.MemoryFilled[i]);
        return r;
    }

    /// <summary>Write per-point frequency deltas (kHz). Returns the NVAPI status (0 = Ok).</summary>
    public static int WriteDeltas(PhysicalGPUHandle handle, uint[] mask, int[] deltasKhz)
    {
        ResolveCurve();
        if (_setBoostTable == null) return -1;
        var s = BoostTableV1.Create(mask);
        for (int i = 0; i < DeltaEntries && i < deltasKhz.Length; i++)
            s.GpuDeltas[i * DeltaStride + 5] = deltasKhz[i];
        try { return _setBoostTable(handle.MemoryAddress, ref s); }
        catch { return -2; }
    }

    /// <summary>True when the private curve entry points resolved on this driver.</summary>
    public static bool CurveAvailable { get { ResolveCurve(); return _getVfpCurve != null && _getBoostTable != null; } }

    // ---- layout probing -------------------------------------------------
    // NvAPIWrapper's V1 curve struct holds only 80 GPU points. An Ada card whose curve runs past
    // that gets silently truncated (a 4070 Ti stops at 945 mV while actually boosting at ~1095 mV).
    // These helpers drive the call with a hand-built buffer so we can try other entry counts and
    // struct versions and see which one the driver actually accepts.

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int RawDelegate(IntPtr gpu, IntPtr payload);
    private static RawDelegate? _rawVfpCurve;

    private const int HeaderBytes = 4 + 16 + 48;   // version + mask[4] + unknown1[12]
    private const int EntryBytes = CurveStride * 4; // 28

    /// <summary>One curve-layout candidate: how many GPU entries and which struct version.</summary>
    public readonly record struct CurveProbe(
        int GpuEntries, int Version, int Status, int ValidPoints, int MinMv, int MaxMv, int MaxMhz);

    /// <summary>
    /// Call GetVFPCurve with a raw buffer sized for <paramref name="gpuEntries"/> points.
    /// Returns the parsed points (voltage mV, frequency MHz) plus the NVAPI status.
    /// </summary>
    public static (int status, (int mv, int mhz)[] points) ReadCurveRaw(
        PhysicalGPUHandle handle, uint[] mask, int gpuEntries, int version,
        int memEntries = MemCurveEntries, int tailUints = 1064)
    {
        ResolveCurve();
        if (_rawVfpCurve == null)
        {
            var ptr = QueryInterface64(0x21537AD4);
            if (ptr == IntPtr.Zero) return (-1, Array.Empty<(int, int)>());
            _rawVfpCurve = Marshal.GetDelegateForFunctionPointer<RawDelegate>(ptr);
        }

        int size = HeaderBytes + (gpuEntries + memEntries) * EntryBytes + tailUints * 4;
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, size | (version << 16));
            for (int i = 0; i < 4; i++) Marshal.WriteInt32(buf, 4 + i * 4, unchecked((int)mask[i]));

            int status;
            try { status = _rawVfpCurve(handle.MemoryAddress, buf); }
            catch { return (-2, Array.Empty<(int, int)>()); }
            if (status != 0) return (status, Array.Empty<(int, int)>());

            // Parse every slot, GPU region and the region after it, so a curve that spills past the
            // GPU array shows up instead of being silently dropped.
            int total = gpuEntries + memEntries;
            var pts = new List<(int, int)>(total);
            for (int i = 0; i < total; i++)
            {
                int off = HeaderBytes + i * EntryBytes;
                int freqKhz = Marshal.ReadInt32(buf, off + 4);
                int voltUv = Marshal.ReadInt32(buf, off + 8);
                if (voltUv > 0 && freqKhz > 0) pts.Add((voltUv / 1000, freqKhz / 1000));
            }
            return (status, pts.ToArray());
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    // The delta table mirrors the curve: same header, then G entries of 9 uints (delta at index 5),
    // then memoryFilled[23], memoryDeltas[23] and a fixed tail. Sized from G the same way.
    private const int DeltaEntryBytes = DeltaStride * 4;   // 36
    private const int DeltaTailBytes = 23 * 4 + 23 * 4 + 1529 * 4;
    private static int DeltaTableSize(int gpuEntries) => HeaderBytes + gpuEntries * DeltaEntryBytes + DeltaTailBytes;

    private static RawDelegate? _rawGetTable, _rawSetTable;

    private static bool ResolveRawTable()
    {
        if (_rawGetTable != null && _rawSetTable != null) return true;
        var g = QueryInterface64(0x23F1B133);
        var s = QueryInterface64(0x0733E009);
        if (g == IntPtr.Zero || s == IntPtr.Zero) return false;
        _rawGetTable = Marshal.GetDelegateForFunctionPointer<RawDelegate>(g);
        _rawSetTable = Marshal.GetDelegateForFunctionPointer<RawDelegate>(s);
        return true;
    }

    /// <summary>
    /// Total curve points a layout carries. The driver's curve runs contiguously through BOTH the
    /// "GPU" array and the 23-entry array after it — 103 points on a 4070 Ti, which is exactly the
    /// mask's bit count. NvAPIWrapper calls the second region "memory" entries; it isn't.
    /// </summary>
    public static int TotalPoints(int gpuEntries) => gpuEntries + MemCurveEntries;

    /// <summary>
    /// Which of the two trailing 23-int arrays holds the deltas for curve points past the GPU array.
    /// 1 = the second array (NvAPIWrapper calls it "MemoryDeltas"), 0 = the first ("MemoryFilled").
    /// Which one the driver actually reads is decided by writing and checking the curve moved.
    /// </summary>
    public static int TrailingArray { get; set; } = 1;

    /// <summary>Byte offset of point i's frequency delta inside the boost table buffer.</summary>
    private static int DeltaOffset(int gpuEntries, int i)
    {
        if (i < gpuEntries) return HeaderBytes + i * DeltaEntryBytes + 20;
        int gpuRegionEnd = HeaderBytes + gpuEntries * DeltaEntryBytes;
        return gpuRegionEnd + TrailingArray * MemCurveEntries * 4 + (i - gpuEntries) * 4;
    }

    /// <summary>Read per-point frequency deltas (kHz) for all points in a layout.</summary>
    public static (int status, int[] deltas) ReadDeltasRaw(
        PhysicalGPUHandle handle, uint[] mask, int gpuEntries, int version)
    {
        if (!ResolveRawTable()) return (-1, Array.Empty<int>());
        int size = DeltaTableSize(gpuEntries);
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, size | (version << 16));
            for (int i = 0; i < 4; i++) Marshal.WriteInt32(buf, 4 + i * 4, unchecked((int)mask[i]));

            int status;
            try { status = _rawGetTable!(handle.MemoryAddress, buf); }
            catch { return (-2, Array.Empty<int>()); }
            if (status != 0) return (status, Array.Empty<int>());

            int total = TotalPoints(gpuEntries);
            var d = new int[total];
            for (int i = 0; i < total; i++) d[i] = Marshal.ReadInt32(buf, DeltaOffset(gpuEntries, i));
            return (status, d);
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>
    /// Write per-point frequency deltas (kHz). Read-modify-write: we Get the table first and only
    /// overwrite the delta fields, so every flag and reserved field the driver cares about is
    /// returned exactly as it gave them. Returns the NVAPI status.
    /// </summary>
    /// <param name="entryFlag">
    /// When &gt;= 0, stamped into each changed entry's leading field. Some driver branches treat it
    /// as "this entry is valid", and ignore entries that leave it at 0.
    /// </param>
    public static int WriteDeltasRaw(
        PhysicalGPUHandle handle, uint[] mask, int gpuEntries, int version, int[] deltasKhz, int entryFlag = -1)
    {
        if (!ResolveRawTable()) return -1;
        int size = DeltaTableSize(gpuEntries);
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, size | (version << 16));
            for (int i = 0; i < 4; i++) Marshal.WriteInt32(buf, 4 + i * 4, unchecked((int)mask[i]));

            // Pull the current table so unknown fields survive the round trip.
            try { _rawGetTable!(handle.MemoryAddress, buf); } catch { }
            Marshal.WriteInt32(buf, 0, size | (version << 16));
            for (int i = 0; i < 4; i++) Marshal.WriteInt32(buf, 4 + i * 4, unchecked((int)mask[i]));

            int total = TotalPoints(gpuEntries);
            for (int i = 0; i < total && i < deltasKhz.Length; i++)
            {
                Marshal.WriteInt32(buf, DeltaOffset(gpuEntries, i), deltasKhz[i]);
                if (entryFlag >= 0 && i < gpuEntries && deltasKhz[i] != 0)
                    Marshal.WriteInt32(buf, HeaderBytes + i * DeltaEntryBytes, entryFlag);
            }

            try { return _rawSetTable!(handle.MemoryAddress, buf); }
            catch { return -2; }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>Raw dump of the two trailing 23-int arrays, to confirm where high-point deltas live.</summary>
    public static (int[] filled, int[] deltas) ReadTrailingArrays(
        PhysicalGPUHandle handle, uint[] mask, int gpuEntries, int version)
    {
        if (!ResolveRawTable()) return (Array.Empty<int>(), Array.Empty<int>());
        int size = DeltaTableSize(gpuEntries);
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, size | (version << 16));
            for (int i = 0; i < 4; i++) Marshal.WriteInt32(buf, 4 + i * 4, unchecked((int)mask[i]));
            try { if (_rawGetTable!(handle.MemoryAddress, buf) != 0) return (Array.Empty<int>(), Array.Empty<int>()); }
            catch { return (Array.Empty<int>(), Array.Empty<int>()); }

            int gpuRegionEnd = HeaderBytes + gpuEntries * DeltaEntryBytes;
            var a = new int[MemCurveEntries]; var b = new int[MemCurveEntries];
            for (int i = 0; i < MemCurveEntries; i++)
            {
                a[i] = Marshal.ReadInt32(buf, gpuRegionEnd + i * 4);
                b[i] = Marshal.ReadInt32(buf, gpuRegionEnd + MemCurveEntries * 4 + i * 4);
            }
            return (a, b);
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>Try a spread of layouts and report what each returns, so the real one can be identified.</summary>
    public static List<CurveProbe> ProbeCurveLayouts(PhysicalGPUHandle handle)
    {
        var results = new List<CurveProbe>();
        foreach (int entries in new[] { 80, 103, 128, 160, 255 })
        {
            foreach (int ver in new[] { 1, 2, 3 })
            {
                var (status, pts) = ReadCurveRaw(handle, FullMask, entries, ver);
                results.Add(new CurveProbe(
                    entries, ver, status, pts.Length,
                    pts.Length > 0 ? pts.Min(p => p.mv) : 0,
                    pts.Length > 0 ? pts.Max(p => p.mv) : 0,
                    pts.Length > 0 ? pts.Max(p => p.mhz) : 0));
            }
        }
        return results;
    }

    public static (int curveBytes, int tableBytes) StructSizes() =>
        (Marshal.SizeOf<VfpCurveV1>(), Marshal.SizeOf<BoostTableV1>());

    /// <summary>
    /// Slot layout differs by generation (from LibreHardwareMonitor):
    ///   RTX 50: [1] = GPU, [2] = memory, no hotspot
    ///   RTX 40: [1] = hotspot, [7] = memory
    ///   older : [1] = hotspot, [9] = memory
    /// </summary>
    public static (double hotspot, double memory) Interpret(string gpuName, double[] slots)
    {
        if (slots.Length < 32) return (double.NaN, double.NaN);
        double Pick(int i) => slots[i] > 0 && slots[i] < 200 ? slots[i] : double.NaN;
        if (gpuName.Contains("RTX 50", StringComparison.OrdinalIgnoreCase)) return (double.NaN, Pick(2));
        if (gpuName.Contains("RTX 40", StringComparison.OrdinalIgnoreCase)) return (Pick(1), Pick(7));
        return (Pick(1), Pick(9));
    }
}
