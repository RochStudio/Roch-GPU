using System.Runtime.InteropServices;

namespace GpuTuner.Core.Backends.Amd;

/// <summary>
/// Raw binding to AMD's Display Library (atiadlxx.dll), Overdrive 8 subset.
///
/// The OD8 tables are read and written through plain integer buffers rather than declared structs.
/// Every one of them is an int array behind a small header, the sizes are known
/// (<see cref="Od8Count"/>), and doing it this way means no struct-packing assumption can silently
/// corrupt a write — the probe that mapped this card used exactly this shape.
/// </summary>
internal static class AdlNative
{
    private const string Dll = "atiadlxx.dll";

    public const int AdlOk = 0;
    public const int AmdVendorId = 1002;

    /// <summary>ADLOD8SettingId member count. AMD's own tools size their tables at Od8Count - 2.</summary>
    public const int Od8Count = 77;
    public const int Od8FeatureCount = Od8Count - 2;

    public delegate IntPtr MemAlloc(int size);
    private static IntPtr Alloc(int size) => Marshal.AllocCoTaskMem(size);
    /// <summary>Held in a static field: the driver keeps calling this long after Create returns.</summary>
    public static readonly MemAlloc Allocator = Alloc;

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Create(MemAlloc cb, int enumConnected, out IntPtr context);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Destroy(IntPtr context);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_NumberOfAdapters_Get(IntPtr context, out int num);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_AdapterInfo_Get(IntPtr context, IntPtr info, int inputSize);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive_Caps(IntPtr context, int adapterIndex, ref int supported, ref int enabled, ref int version);

    // lpNumberOfFeatures is IN/OUT: it must arrive pre-set to the table size the caller expects.
    // Passing 0 makes the driver return ADL_ERR_NULL_POINTER, which reads like "unsupported" but isn't.
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Init_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int caps, ref int numFeatures, ref IntPtr initSettingList);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int numFeatures, ref IntPtr currentList);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Setting_Set(IntPtr context, int adapterIndex,
        IntPtr setSetting, IntPtr currentSetting);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_New_QueryPMLogData_Get(IntPtr context, int adapterIndex, IntPtr dataOutput);

    // Identity. All three are optional — older or trimmed drivers may not export them, which surfaces
    // as EntryPointNotFoundException, so every call site treats a failure as "unknown", not an error.
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Graphics_VersionsX2_Get(IntPtr context, IntPtr versionsInfo);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Graphics_Versions_Get(IntPtr context, IntPtr versionsInfo);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_VideoBiosInfo_Get(IntPtr context, int adapterIndex, IntPtr biosInfo);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_MemoryInfo2_Get(IntPtr context, int adapterIndex, IntPtr memoryInfo);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_MemoryInfo_Get(IntPtr context, int adapterIndex, IntPtr memoryInfo);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
    public struct AdapterInfo
    {
        public int iSize;
        public int iAdapterIndex;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strUDID;
        public int iBusNumber;
        public int iDeviceNumber;
        public int iFunctionNumber;
        public int iVendorID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strAdapterName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDisplayName;
        public int iPresent;
        public int iExist;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPath;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPathExt;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strPNPString;
        public int iOSDisplayIndex;
    }

    public static string Describe(int rc) => rc switch
    {
        0 => "OK",
        -1 => "generic error",
        -2 => "ADL not initialised",
        -3 => "invalid parameter",
        -4 => "invalid parameter size",
        -5 => "invalid adapter index",
        -8 => "not supported",
        -9 => "null pointer",
        -10 => "adapter disabled",
        _ => $"error {rc}"
    };
}

/// <summary>ADLOD8SettingId — index into every OD8 table. Verbatim from adl_defines.h.</summary>
internal enum Od8Id
{
    GfxClkFMax = 0,
    GfxClkFMin = 1,
    UClkFMax = 8,
    PowerPercentage = 9,
    FanMinSpeed = 10,
    FanAcousticLimit = 11,
    FanTargetTemp = 12,
    OperatingTempMax = 13,
    AcTiming = 14,
    FanZeroRpmControl = 15,
    AutoUvEngineControl = 16,
    AutoOcEngineControl = 17,
    AutoOcMemoryControl = 18,
    FanCurveTemperature1 = 19,
    FanCurveSpeed1 = 20,
    FanCurveTemperature2 = 21,
    FanCurveSpeed2 = 22,
    FanCurveTemperature3 = 23,
    FanCurveSpeed3 = 24,
    FanCurveTemperature4 = 25,
    FanCurveSpeed4 = 26,
    FanCurveTemperature5 = 27,
    FanCurveSpeed5 = 28,
    UClkFMin = 34,
    OptimizedPowerMode = 36,
    OdVoltage = 37,
    TdcPercentage = 47
}

/// <summary>ADLOD8FeatureControl — the capability bitmask returned alongside the init table.</summary>
[Flags]
internal enum Od8Feature
{
    GfxClkLimits = 1 << 0,
    GfxClkCurve = 1 << 1,
    UClkMax = 1 << 2,
    PowerLimit = 1 << 3,
    AcousticLimitSclk = 1 << 4,
    FanSpeedMin = 1 << 5,
    TemperatureFan = 1 << 6,
    TemperatureSystem = 1 << 7,
    MemoryTimingTune = 1 << 8,
    FanZeroRpmControl = 1 << 9,
    AutoUvEngine = 1 << 10,
    AutoOcEngine = 1 << 11,
    AutoOcMemory = 1 << 12,
    FanCurve = 1 << 13,
    OptimizedGpuPowerMode = 1 << 16,
    OdVoltageLimit = 1 << 17,
    GfxVoltageLimit = 1 << 21,
    TdcLimit = 1 << 22,
    FullControlMode = 1 << 23,
    PowerGauge = 1 << 28
}

/// <summary>ADLSensorType — indices into the PMLog output array.</summary>
internal enum PmLog
{
    CoreClockMhz = 1,
    MemoryClockMhz = 2,
    TemperatureEdge = 8,
    TemperatureMemory = 9,
    FanRpm = 14,
    FanPercent = 15,
    ActivityGfx = 19,
    ActivityMem = 20,
    GfxVoltageMv = 21,
    AsicPowerW = 23,
    TemperatureHotspot = 27,
    GfxPowerW = 30,
    ThrottlerStatus = 35,
    DgpuPowerLimitW = 49,
    BoardPowerW = 73,
    MaxSensors = 256
}
