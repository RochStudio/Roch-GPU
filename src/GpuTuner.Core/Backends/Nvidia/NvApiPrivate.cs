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

        // Both regions, not just the "GPU" one: the curve does not stop at the 80-entry array, and
        // stopping there truncated a 4070 Ti at 945 mV instead of 1090.
        //
        // This reader still cannot see the whole curve, because the struct's fields only span 103 of
        // the buffer's points — a 5070 Ti has 127 and this returns the first 103, ending at 1090 mV
        // instead of 1240. It is a fallback for drivers that refuse the raw call; ReadCurveRaw is
        // what normally answers and it parses the buffer to the mask. Anything measured against the
        // short version (the stock ceiling, an undervolt target) comes out against the wrong top,
        // so callers must prefer the raw reader. See TotalPoints vs StructPoints.
        var r = new (uint, int)[StructPoints(CurveEntries)];
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
        var r = new int[StructPoints(DeltaEntries)];   // struct-bounded, like ReadCurve above
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
        int memEntries = MemCurveEntries, int tailUints = CurveTailUints)
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
            int total = Math.Min(MaskPoints, (size - HeaderBytes) / EntryBytes);
            var pts = new List<(int, int)>(total);
            int lastUv = 0;
            for (int i = 0; i < total; i++)
            {
                int off = HeaderBytes + i * EntryBytes;
                int freqKhz = Marshal.ReadInt32(buf, off + 4);
                int voltUv = Marshal.ReadInt32(buf, off + 8);
                if (voltUv <= 0 || freqKhz <= 0) continue;
                // The buffer carries more than one clock domain. The graphics curve climbs the whole
                // way, so a point that steps back down in voltage is the next domain starting, not a
                // continuation — on a 5070 Ti that is slot 127, at 515 mV and the 405 MHz memory
                // clock, sitting immediately after the graphics curve's 1240 mV top.
                if (voltUv < lastUv) break;
                lastUv = voltUv;
                pts.Add((voltUv / 1000, freqKhz / 1000));
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
    /// Points the <em>named struct fields</em> reach: the "GPU" array plus the 23-entry one after it.
    /// 103 for the 80-entry layout. This is a property of NvAPIWrapper's struct declaration, not of
    /// the card — only the struct-based readers are limited to it.
    /// </summary>
    private static int StructPoints(int gpuEntries) => gpuEntries + MemCurveEntries;

    /// <summary>
    /// Points the curve buffer really carries, which is what the raw readers and every writer
    /// address. Bounded by the mask, since a point outside it can never be selected.
    ///
    /// This used to return <see cref="StructPoints"/>, on the reasoning that a 4070 Ti's 103 points
    /// were "exactly the mask's bit count". They were not: the mask is 128 bits and that card simply
    /// had 103 points. A 5070 Ti returns 127 in the same buffer — one contiguous run from 450 mV to
    /// 1240 mV with no discontinuity at index 102 — so the struct's split reaches only 81% of it,
    /// and sizing a delta array by it left the top 25 slots unwritten on every reset.
    /// </summary>
    public static int TotalPoints(int gpuEntries) =>
        Math.Min(MaskPoints, (CurveTableSize(gpuEntries) - HeaderBytes) / EntryBytes);

    /// <summary>Bytes the curve call wants, for the layout ReadCurveRaw builds.</summary>
    private static int CurveTableSize(int gpuEntries) =>
        HeaderBytes + (gpuEntries + MemCurveEntries) * EntryBytes + CurveTailUints * 4;

    /// <summary>
    /// Points the curve buffer really carries. NvAPIWrapper's struct splits the array into an
    /// 80-entry "GPU" region and a 23-entry one after it, and stopping where those end truncates the
    /// curve: a 5070 Ti returns a contiguous run to 1240 mV, of which the split layout shows the
    /// first 103 (to 1090 mV) and treats the rest as padding. The selection mask is 128 bits wide,
    /// and that — not the struct's internal division — is the real bound.
    /// </summary>
    public const int MaskPoints = 128;

    /// <summary>Uints of tail past the named entry arrays. The driver fills curve points into it.</summary>
    private const int CurveTailUints = 1064;

    /// <summary>Delta entries the boost table holds: the mask's width, bounded by the buffer.</summary>
    private static int DeltaPoints(int gpuEntries) =>
        Math.Min(MaskPoints, (DeltaTableSize(gpuEntries) - HeaderBytes) / DeltaEntryBytes);

    /// <summary>
    /// Which of the two trailing 23-int arrays holds the deltas for curve points past the GPU array.
    /// 1 = the second array (NvAPIWrapper calls it "MemoryDeltas"), 0 = the first ("MemoryFilled").
    /// Which one the driver actually reads is decided by writing and checking the curve moved.
    /// </summary>
    public static int TrailingArray { get; set; } = 1;

    /// <summary>
    /// Byte offset of point i's frequency delta inside the boost table buffer.
    ///
    /// One flat array, 36 bytes per entry, all the way. The struct this was modelled on splits after
    /// 80 entries into two 23-entry arrays of bare ints, and writing points past 80 into those puts
    /// them inside entry 80's record instead — the driver validates those fields and rejects the
    /// whole call, taking the valid points down with it. Verified by driving a known +150 MHz offset
    /// through the table: 127 entries moved, every one at stride 0x24 + 20, nothing anywhere else.
    /// </summary>
    private static int DeltaOffset(int gpuEntries, int i) => HeaderBytes + i * DeltaEntryBytes + 20;

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

            int total = DeltaPoints(gpuEntries);
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

            int total = DeltaPoints(gpuEntries);
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

    // ------------------------------------------------------------------ voltage rails
    //
    // NVVDD (the core rail) and MSVDD (the rail that feeds XBAR/SYS/video) are exposed through a
    // separate private family from the V/F curve. Same trap as the curve: the calls succeed with a
    // zeroed mask and hand back an empty structure, so the rail mask from GetInfo has to be written
    // into the Status and Control buffers before calling them.
    //
    // Entry layouts were confirmed against this hardware rather than assumed: MSVDD carried a
    // -50000 uV primary-maximum offset against a 1055000 uV limit, and the tool that set it displayed
    // exactly 1.005 V.

    private const uint FnVoltRailsGetInfo = 0x2C73AFDC, FnVoltRailsGetStatus = 0x5D0634EE,
                       FnVoltRailsGetControl = 0xA3070DB0, FnVoltRailsSetControl = 0x87C55C8A;
    private const int VoltRailsInfoSize = 0x184C, VoltRailsStatusSize = 0x1620, VoltRailsControlSize = 0x0AC8;
    private const int VoltRailsVersion = 2;
    private const int RailMaskOffset = 0x04;
    private const int StatusEntries = 0xA0, StatusStride = 0xAC;
    private const int ControlEntries = 0x48, ControlStride = 0x54;
    /// <summary>Signed microvolt offset applied to the rail's primary maximum.</summary>
    private const int ControlMaxOffset = 0x04;
    /// <summary>
    /// Signed microvolt offset applied to the rail's ALTERNATE maximum — and this is the one the
    /// boost algorithm obeys. Moving the primary alone raises the number the rail reports while the
    /// card carries on selecting the old voltage: measured here, primary at 1100 mV with the
    /// alternate left at 1055 peaked at 1045 mV under load, and raising both reached 1095 mV.
    /// </summary>
    private const int ControlAltMaxOffset = 0x08;
    /// <summary>Signed microvolt offset applied to the rail's minimum.</summary>
    private const int ControlMinOffset = 0x10;

    /// <summary>Rail indices in the mask: 0 is the core supply, 1 the one behind XBAR/SYS/video.</summary>
    public const int RailNvvdd = 0, RailMsvdd = 1;

    /// <summary>One power rail's live state, in microvolts.</summary>
    public readonly record struct VoltRail(int Index, uint CurrentUv, uint MaxUv, uint MinUv, int MaxOffsetUv, int MinOffsetUv);

    private static RawDelegate? Resolve(uint id)
    {
        var p = QueryInterface64(id);
        return p == IntPtr.Zero ? null : Marshal.GetDelegateForFunctionPointer<RawDelegate>(p);
    }

    /// <summary>Call one of the rail entry points into a caller-sized buffer. Returns null on failure.</summary>
    private static byte[]? RailCall(PhysicalGPUHandle handle, uint id, int size, uint mask)
    {
        var fn = Resolve(id);
        if (fn == null) return null;
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, size | (VoltRailsVersion << 16));
            if (mask != 0) Marshal.WriteInt32(buf, RailMaskOffset, unchecked((int)mask));
            int status;
            try { status = fn(handle.MemoryAddress, buf); }
            catch { return null; }
            if (status != 0) return null;
            var outb = new byte[size];
            Marshal.Copy(buf, outb, 0, size);
            return outb;
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>The mask of rails this GPU exposes; 0 when the family is unavailable.</summary>
    public static uint ReadRailMask(PhysicalGPUHandle handle) =>
        RailCall(handle, FnVoltRailsGetInfo, VoltRailsInfoSize, 0) is { } info
            ? BitConverter.ToUInt32(info, RailMaskOffset)
            : 0u;

    /// <summary>Read every rail the GPU reports. Empty when unsupported.</summary>
    public static IReadOnlyList<VoltRail> ReadVoltRails(PhysicalGPUHandle handle)
    {
        uint mask = ReadRailMask(handle);
        if (mask == 0) return Array.Empty<VoltRail>();
        var status = RailCall(handle, FnVoltRailsGetStatus, VoltRailsStatusSize, mask);
        var control = RailCall(handle, FnVoltRailsGetControl, VoltRailsControlSize, mask);
        if (status == null) return Array.Empty<VoltRail>();

        var rails = new List<VoltRail>();
        for (int r = 0, slot = 0; r < 32; r++)
        {
            if ((mask & (1u << r)) == 0) continue;
            int s = StatusEntries + slot * StatusStride;
            int c = ControlEntries + slot * ControlStride;
            slot++;
            if (s + StatusStride > status.Length) break;
            rails.Add(new VoltRail(
                r,
                BitConverter.ToUInt32(status, s + 0x04),
                BitConverter.ToUInt32(status, s + 0x08),
                BitConverter.ToUInt32(status, s + 0x18),
                control != null && c + ControlStride <= control.Length ? BitConverter.ToInt32(control, c + ControlMaxOffset) : 0,
                control != null && c + ControlStride <= control.Length ? BitConverter.ToInt32(control, c + ControlMinOffset) : 0));
        }
        return rails;
    }

    /// <summary>
    /// Apply a signed microvolt offset to a rail's minimum, raising or lowering the floor it is
    /// allowed to drop to. Only one field, unlike the maximum: verified on hardware by moving
    /// NVVDD's floor 800 -> 850 mV and back, with MSVDD's floor unmoved throughout.
    /// </summary>
    public static int WriteVoltRailMinOffset(PhysicalGPUHandle handle, int railIndex, int offsetUv)
    {
        uint mask = ReadRailMask(handle);
        if (mask == 0 || (mask & (1u << railIndex)) == 0) return -1;
        var fnSet = Resolve(FnVoltRailsSetControl);
        if (fnSet == null) return -1;

        int slot = 0;
        for (int r = 0; r < railIndex; r++) if ((mask & (1u << r)) != 0) slot++;

        var buf = Marshal.AllocHGlobal(VoltRailsControlSize);
        try
        {
            for (int i = 0; i < VoltRailsControlSize; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, VoltRailsControlSize | (VoltRailsVersion << 16));
            Marshal.WriteInt32(buf, RailMaskOffset, unchecked((int)mask));
            var get = Resolve(FnVoltRailsGetControl);
            if (get != null) { try { get(handle.MemoryAddress, buf); } catch { } }
            Marshal.WriteInt32(buf, 0, VoltRailsControlSize | (VoltRailsVersion << 16));
            Marshal.WriteInt32(buf, RailMaskOffset, unchecked((int)mask));
            Marshal.WriteInt32(buf, ControlEntries + slot * ControlStride + ControlMinOffset, offsetUv);
            try { return fnSet(handle.MemoryAddress, buf); }
            catch { return -2; }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>
    /// Apply a signed microvolt offset to a rail's maximum, primary and alternate together — the
    /// alternate is what actually binds, so moving one without the other changes the reported
    /// ceiling and nothing else. Read-modify-write: the current control block is fetched and only
    /// these two fields changed, so every reserved field the driver validates goes back exactly as
    /// it arrived. Returns the NVAPI status (0 = Ok).
    /// </summary>
    public static int WriteVoltRailMaxOffset(PhysicalGPUHandle handle, int railIndex, int offsetUv)
    {
        uint mask = ReadRailMask(handle);
        if (mask == 0 || (mask & (1u << railIndex)) == 0) return -1;
        var fnSet = Resolve(FnVoltRailsSetControl);
        if (fnSet == null) return -1;

        int slot = 0;
        for (int r = 0; r < railIndex; r++) if ((mask & (1u << r)) != 0) slot++;

        var buf = Marshal.AllocHGlobal(VoltRailsControlSize);
        try
        {
            for (int i = 0; i < VoltRailsControlSize; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, VoltRailsControlSize | (VoltRailsVersion << 16));
            Marshal.WriteInt32(buf, RailMaskOffset, unchecked((int)mask));
            var get = Resolve(FnVoltRailsGetControl);
            if (get != null) { try { get(handle.MemoryAddress, buf); } catch { } }
            Marshal.WriteInt32(buf, 0, VoltRailsControlSize | (VoltRailsVersion << 16));
            Marshal.WriteInt32(buf, RailMaskOffset, unchecked((int)mask));
            int entry = ControlEntries + slot * ControlStride;
            Marshal.WriteInt32(buf, entry + ControlMaxOffset, offsetUv);
            Marshal.WriteInt32(buf, entry + ControlAltMaxOffset, offsetUv);
            try { return fnSet(handle.MemoryAddress, buf); }
            catch { return -2; }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    // ------------------------------------------------------------------ crossbar (XBAR)
    //
    // The GPU's interconnect clock, which NVAPI's public surface does not expose at all: it is not a
    // PStates20 domain (writes to every domain id but graphics and memory are refused) and does not
    // appear in the clock-domain enumeration. It has its own control family, and its own hardware
    // frequency counter — MeasureClockKhz — which is what makes a write verifiable rather than
    // merely acknowledged.

    private const uint FnXbarGetInfo = 0x57B5A5DF, FnXbarGetControl = 0xF58938F5,
                       FnXbarSetControl = 0xD14B69CF, FnXbarMeasure = 0x527FC458;
    private const int XbarInfoSize = 0x86AC, XbarControlSize = 0x61A4, XbarMeasureSize = 0x000C;
    private const uint XbarInfoVersion = 0x000486AC, XbarControlVersion = 0x000261A4, XbarMeasureVersion = 0x0001000C;
    private const int XbarInfoEntries = 0xB0, XbarInfoStride = 0x430;
    /// <summary>Window searched for the int16 offset-range pair inside a type-1 entry.</summary>
    private const int XbarRangeSearchStart = 0x38, XbarRangeSearchEnd = 0x40;
    /// <summary>The control request carries a 2 here; it comes back empty without it.</summary>
    private const int XbarControlSelector = 0x08;
    private const int XbarControlOffsetKhz = 0x53C;

    /// <summary>Clock domain indices accepted by <see cref="MeasureClockKhz"/>, confirmed on hardware.</summary>
    public const int DomainCore = 0, DomainXbar = 1, DomainMemory = 4;

    /// <summary>Crossbar offset range the driver reports; Supported is false when the family is absent.</summary>
    public readonly record struct XbarInfo(bool Supported, int MinOffsetMhz, int MaxOffsetMhz);

    private static byte[]? XbarCall(PhysicalGPUHandle handle, uint id, int size, uint version, bool selector)
    {
        var fn = Resolve(id);
        if (fn == null) return null;
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int i = 0; i < size; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, unchecked((int)version));
            if (selector) Marshal.WriteInt32(buf, XbarControlSelector, 2);
            int status;
            try { status = fn(handle.MemoryAddress, buf); }
            catch { return null; }
            if (status != 0) return null;
            var outb = new byte[size];
            Marshal.Copy(buf, outb, 0, size);
            return outb;
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>
    /// Measure a clock domain with the GPU's own counter, in kHz; 0 when unavailable. This reports
    /// what the domain is really running, not the setpoint it was asked for.
    /// </summary>
    public static uint MeasureClockKhz(PhysicalGPUHandle handle, int domain)
    {
        var fn = Resolve(FnXbarMeasure);
        if (fn == null) return 0;
        var buf = Marshal.AllocHGlobal(XbarMeasureSize);
        try
        {
            for (int i = 0; i < XbarMeasureSize; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, unchecked((int)XbarMeasureVersion));
            Marshal.WriteInt32(buf, 0x04, domain);
            int status;
            try { status = fn(handle.MemoryAddress, buf); }
            catch { return 0; }
            return status == 0 ? (uint)Marshal.ReadInt32(buf, 0x08) : 0u;
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>Read the crossbar offset range. The entry is found by type rather than by index.</summary>
    public static XbarInfo ReadXbarInfo(PhysicalGPUHandle handle)
    {
        var info = XbarCall(handle, FnXbarGetInfo, XbarInfoSize, XbarInfoVersion, selector: false);
        if (info == null) return default;
        for (int i = 0; i < 32; i++)
        {
            int e = XbarInfoEntries + i * XbarInfoStride;
            if (e + XbarRangeSearchEnd + 4 > info.Length) break;
            if (BitConverter.ToUInt32(info, e) != 1u) continue;

            // The range is two adjacent int16s, and its offset within the entry is not the same on
            // every driver: a 5070 Ti (610.88) carries it at +0x3C, a 4070 Ti (591.86) at +0x3A, both
            // reading -1000..+1000. Reading one fixed offset therefore found (1000, 0) on Ada, failed
            // min > max, and reported a working crossbar as unsupported. Locate the pair instead.
            //
            // A candidate has to look like an offset range — negative floor, positive ceiling, neither
            // absurd — which is tight enough that the neighbouring words here (3598, 1024, 258, 15000)
            // cannot pass for one.
            for (int off = XbarRangeSearchStart; off <= XbarRangeSearchEnd; off += 2)
            {
                short min = BitConverter.ToInt16(info, e + off);
                short max = BitConverter.ToInt16(info, e + off + 2);
                if (min < 0 && max > 0 && min >= -2000 && max <= 2000)
                    return new XbarInfo(true, min, max);
            }
            return default;      // right entry, unrecognised layout: say so rather than guess a range
        }
        return default;
    }

    /// <summary>Crossbar offset currently applied, in MHz.</summary>
    public static int ReadXbarOffsetMhz(PhysicalGPUHandle handle) =>
        XbarCall(handle, FnXbarGetControl, XbarControlSize, XbarControlVersion, selector: true) is { } c
            ? BitConverter.ToInt32(c, XbarControlOffsetKhz) / 1000
            : 0;

    /// <summary>Apply a crossbar offset in MHz. Read-modify-write. Returns the NVAPI status (0 = Ok).</summary>
    public static int WriteXbarOffsetMhz(PhysicalGPUHandle handle, int mhz)
    {
        var fnSet = Resolve(FnXbarSetControl);
        var fnGet = Resolve(FnXbarGetControl);
        if (fnSet == null || fnGet == null) return -1;
        var buf = Marshal.AllocHGlobal(XbarControlSize);
        try
        {
            for (int i = 0; i < XbarControlSize; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, unchecked((int)XbarControlVersion));
            Marshal.WriteInt32(buf, XbarControlSelector, 2);
            try { fnGet(handle.MemoryAddress, buf); } catch { }
            Marshal.WriteInt32(buf, 0, unchecked((int)XbarControlVersion));
            Marshal.WriteInt32(buf, XbarControlSelector, 2);
            Marshal.WriteInt32(buf, XbarControlOffsetKhz, mhz * 1000);
            try { return fnSet(handle.MemoryAddress, buf); }
            catch { return -2; }
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    /// <summary>
    /// Why the crossbar came back unsupported. ReadXbarInfo collapses three different failures into
    /// one "no": the entry points not resolving at all, the call returning a status, or the call
    /// succeeding with a layout that carries no type-1 entry. On a driver or architecture other than
    /// the one the offsets were mapped against, knowing which of those happened is the whole question.
    ///
    /// Read-only — GetInfo, GetControl and the frequency counter only, never SetControl.
    /// </summary>
    public readonly record struct XbarProbe(
        bool InfoResolved, bool ControlResolved, bool SetResolved, bool MeasureResolved,
        int InfoStatus, int ControlStatus,
        uint CoreKhz, uint XbarKhz, uint MemoryKhz,
        int[] EntryTypes,
        int TypeOneIndex,
        int[] TypeOneWords,
        int[] ControlWords);

    /// <summary>
    /// Call a private entry point read-only with a deliberately oversized buffer. The declared size
    /// stays whatever we are probing, but the allocation is far larger, so a driver that writes more
    /// than we declared lands inside our own memory rather than past it.
    /// </summary>
    private static (int status, byte[] data)? ProbeCall(PhysicalGPUHandle handle, uint id, int declaredSize,
                                                        uint version, int selectorValue = -1)
    {
        var fn = Resolve(id);
        if (fn == null) return null;
        const int Slack = 0x40000;                       // 256 KB, far past any of these structures
        var buf = Marshal.AllocHGlobal(declaredSize + Slack);
        try
        {
            for (int i = 0; i < declaredSize + Slack; i += 4) Marshal.WriteInt32(buf, i, 0);
            Marshal.WriteInt32(buf, 0, unchecked((int)version));
            if (selectorValue >= 0) Marshal.WriteInt32(buf, XbarControlSelector, selectorValue);
            int status;
            try { status = fn(handle.MemoryAddress, buf); }
            catch { return (int.MinValue, Array.Empty<byte>()); }
            var outb = new byte[declaredSize];
            Marshal.Copy(buf, outb, 0, declaredSize);
            return (status, outb);
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    public static XbarProbe ProbeXbar(PhysicalGPUHandle handle)
    {
        bool infoR = QueryInterface64(FnXbarGetInfo) != IntPtr.Zero;
        bool ctrlR = QueryInterface64(FnXbarGetControl) != IntPtr.Zero;
        bool setR = QueryInterface64(FnXbarSetControl) != IntPtr.Zero;
        bool measR = QueryInterface64(FnXbarMeasure) != IntPtr.Zero;

        var info = ProbeCall(handle, FnXbarGetInfo, XbarInfoSize, XbarInfoVersion);
        var ctrl = ProbeCall(handle, FnXbarGetControl, XbarControlSize, XbarControlVersion, selectorValue: 2);

        // Whatever the info call returned, list the entry type words: the reader looks for a 1, and
        // seeing what is actually there says whether the stride is wrong or the family is just absent.
        var types = new List<int>();
        if (info is { status: 0, data: var d })
            for (int i = 0; i < 32; i++)
            {
                int e = XbarInfoEntries + i * XbarInfoStride;
                if (e + 4 > d.Length) break;
                types.Add(BitConverter.ToInt32(d, e));
            }

        // The reader locates the range at a fixed offset inside the type-1 entry. If that offset is
        // wrong for this driver it rejects a perfectly good entry, so dump the entry's leading words
        // and let the actual layout be read off rather than assumed.
        int oneIdx = types.IndexOf(1);
        var oneWords = new List<int>();
        if (oneIdx >= 0 && info is { status: 0, data: var d2 })
        {
            int e = XbarInfoEntries + oneIdx * XbarInfoStride;
            for (int off = 0; off < 0x80 && e + off + 4 <= d2.Length; off += 4)
                oneWords.Add(BitConverter.ToInt32(d2, e + off));
        }

        // The control buffer's layout matters as much as the info one: the offset field is written at a
        // fixed position, and if that has moved too then a non-zero write lands in a field the driver
        // validates and the whole call is refused. Everything around the assumed position reads zero,
        // so record where the buffer is NOT zero instead and let the structure show itself.
        var ctrlWords = new List<int>();
        if (ctrl is { status: 0, data: var d3 })
            for (int off = 0; off + 4 <= d3.Length; off += 4)
            {
                int w = BitConverter.ToInt32(d3, off);
                if (w != 0) { ctrlWords.Add(off); ctrlWords.Add(w); }
                if (ctrlWords.Count >= 240) break;
            }

        return new XbarProbe(
            infoR, ctrlR, setR, measR,
            info?.status ?? int.MinValue, ctrl?.status ?? int.MinValue,
            MeasureClockKhz(handle, DomainCore),
            MeasureClockKhz(handle, DomainXbar),
            MeasureClockKhz(handle, DomainMemory),
            types.ToArray(), oneIdx, oneWords.ToArray(), ctrlWords.ToArray());
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
