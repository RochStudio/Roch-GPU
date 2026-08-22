using System.Runtime.InteropServices;

namespace GpuTuner.Core.Backends.Nvidia;

/// <summary>
/// Minimal NVML binding for locking the graphics clock range — the same thing
/// <c>nvidia-smi --lock-gpu-clocks</c> does.
///
/// This is a *public, documented* driver API, unlike the private clock-boost lock, so it is the
/// dependable way to hold the core at a chosen point on the V/F curve: cap the clock at the
/// frequency the curve reaches at the target voltage and the card stops asking for more volts.
/// Requires administrator rights (the app manifest asks for them) and driver r465 or newer.
///
/// nvml.dll lives beside the driver in System32, so it is resolved by name with no path juggling.
/// </summary>
internal static class Nvml
{
    private const string Dll = "nvml.dll";

    // NVML return codes we care about; everything non-zero is a failure.
    private const int Success = 0;

    [DllImport(Dll, EntryPoint = "nvmlInit_v2")] private static extern int Init();
    [DllImport(Dll, EntryPoint = "nvmlShutdown")] private static extern int Shutdown();
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetHandleByIndex_v2")] private static extern int GetHandle(uint index, out IntPtr device);
    [DllImport(Dll, EntryPoint = "nvmlDeviceSetGpuLockedClocks")] private static extern int SetLocked(IntPtr device, uint minMhz, uint maxMhz);
    [DllImport(Dll, EntryPoint = "nvmlDeviceResetGpuLockedClocks")] private static extern int ResetLocked(IntPtr device);
    [DllImport(Dll, EntryPoint = "nvmlErrorString")] private static extern IntPtr ErrorString(int result);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetName")] private static extern int GetNameRaw(IntPtr device, [Out] byte[] name, uint length);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetPowerUsage")] private static extern int GetPowerUsage(IntPtr device, out uint milliwatts);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMaxClockInfo")] private static extern int GetMaxClock(IntPtr device, int type, out uint mhz);

    private static bool _initTried, _initOk;
    private static readonly object Gate = new();

    /// <summary>Last failure, for diagnostics. Null while everything is fine.</summary>
    public static string? LastError { get; private set; }

    /// <summary>True once nvml.dll has loaded and initialised. Safe to call repeatedly.</summary>
    public static bool IsAvailable
    {
        get
        {
            lock (Gate)
            {
                if (_initTried) return _initOk;
                _initTried = true;
                try
                {
                    int r = Init();
                    _initOk = r == Success;
                    if (!_initOk) LastError = $"nvmlInit failed: {Describe(r)}";
                }
                catch (DllNotFoundException) { LastError = "nvml.dll not found (is the NVIDIA driver installed?)"; }
                catch (EntryPointNotFoundException e) { LastError = "nvml.dll is too old: " + e.Message; }
                catch (Exception e) { LastError = e.Message; }
                return _initOk;
            }
        }
    }

    /// <summary>Pin the graphics clock to [min, max] MHz. Returns null on success, else the reason.</summary>
    public static string? LockGraphicsClocks(int gpuIndex, int minMhz, int maxMhz)
    {
        if (!IsAvailable) return LastError ?? "NVML unavailable";
        lock (Gate)
        {
            try
            {
                int r = GetHandle((uint)gpuIndex, out var dev);
                if (r != Success) return $"nvmlDeviceGetHandleByIndex: {Describe(r)}";
                r = SetLocked(dev, (uint)Math.Max(0, minMhz), (uint)Math.Max(0, maxMhz));
                return r == Success ? null : $"nvmlDeviceSetGpuLockedClocks({minMhz},{maxMhz}): {Describe(r)}";
            }
            catch (Exception e) { return e.Message; }
        }
    }

    /// <summary>Hand the clock range back to the driver. Returns null on success, else the reason.</summary>
    public static string? ResetGraphicsClocks(int gpuIndex)
    {
        if (!IsAvailable) return LastError ?? "NVML unavailable";
        lock (Gate)
        {
            try
            {
                int r = GetHandle((uint)gpuIndex, out var dev);
                if (r != Success) return $"nvmlDeviceGetHandleByIndex: {Describe(r)}";
                r = ResetLocked(dev);
                return r == Success ? null : $"nvmlDeviceResetGpuLockedClocks: {Describe(r)}";
            }
            catch (Exception e) { return e.Message; }
        }
    }

    /// <summary>Device name as NVML sees it — used to confirm the index lines up with the NVAPI one.</summary>
    /// <summary>
    /// Board power draw in watts, or NaN when NVML cannot say.
    ///
    /// NVAPI has no watts on this card: its power families report per-cent-mille of the limit and
    /// nothing else — the topology call, the policy status and the policy info all agree on that,
    /// and a sweep of every struct size each accepts turned up no milliwatt field anywhere. NVML
    /// reports the draw directly, so that is where this comes from.
    /// </summary>
    public static double PowerWatts(int gpuIndex)
    {
        if (!IsAvailable) return double.NaN;
        lock (Gate)
        {
            try
            {
                if (GetHandle((uint)gpuIndex, out var dev) != Success) return double.NaN;
                return GetPowerUsage(dev, out uint mw) == Success ? mw / 1000.0 : double.NaN;
            }
            catch (Exception) { return double.NaN; }
        }
    }

    /// <summary>
    /// Highest graphics clock the driver will report, in MHz; 0 when NVML cannot say. Used as the top
    /// of the clock-range control, so the slider stops where the card does.
    /// </summary>
    public static int MaxGraphicsClockMhz(int gpuIndex)
    {
        if (!IsAvailable) return 0;
        lock (Gate)
        {
            try
            {
                if (GetHandle((uint)gpuIndex, out var dev) != Success) return 0;
                return GetMaxClock(dev, ClockGraphics, out uint mhz) == Success ? (int)mhz : 0;
            }
            catch (Exception) { return 0; }
        }
    }

    /// <summary>NVML_CLOCK_GRAPHICS.</summary>
    private const int ClockGraphics = 0;

    public static string? DeviceName(int gpuIndex)
    {
        if (!IsAvailable) return null;
        lock (Gate)
        {
            try
            {
                if (GetHandle((uint)gpuIndex, out var dev) != Success) return null;
                var buf = new byte[96];
                if (GetNameRaw(dev, buf, (uint)buf.Length) != Success) return null;
                int len = Array.IndexOf(buf, (byte)0);
                return System.Text.Encoding.UTF8.GetString(buf, 0, len < 0 ? buf.Length : len);
            }
            catch { return null; }
        }
    }

    private static string Describe(int result)
    {
        try
        {
            var p = ErrorString(result);
            var s = p == IntPtr.Zero ? null : Marshal.PtrToStringAnsi(p);
            return string.IsNullOrEmpty(s) ? $"error {result}" : $"{s} ({result})";
        }
        catch { return $"error {result}"; }
    }

    public static void TryShutdown()
    {
        lock (Gate)
        {
            if (!_initOk) return;
            try { Shutdown(); } catch { }
            _initOk = false; _initTried = false;
        }
    }
}
