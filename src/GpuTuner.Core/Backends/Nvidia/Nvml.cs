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
///
/// One session per process, and it does not outlive a display driver reset — see
/// <see cref="SessionMayBeStale"/> for what that looked like and how it is recovered.
/// </summary>
internal static class Nvml
{
    private const string Dll = "nvml.dll";

    // NVML return codes. Success, plus the four that say this process's session no longer describes
    // the card, rather than the request being wrong.
    private const int Success = 0;
    private const int Uninitialized = 1;
    private const int GpuIsLost = 15;
    private const int ResetRequired = 16;
    private const int Unknown = 999;

    /// <summary>NVML_CLOCK_GRAPHICS.</summary>
    private const int ClockGraphics = 0;

    [DllImport(Dll, EntryPoint = "nvmlInit_v2")] private static extern int Init();
    [DllImport(Dll, EntryPoint = "nvmlShutdown")] private static extern int Shutdown();
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetHandleByIndex_v2")] private static extern int GetHandle(uint index, out IntPtr device);
    [DllImport(Dll, EntryPoint = "nvmlDeviceSetGpuLockedClocks")] private static extern int SetLocked(IntPtr device, uint minMhz, uint maxMhz);
    [DllImport(Dll, EntryPoint = "nvmlDeviceResetGpuLockedClocks")] private static extern int ResetLocked(IntPtr device);
    [DllImport(Dll, EntryPoint = "nvmlErrorString")] private static extern IntPtr ErrorString(int result);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetName")] private static extern int GetNameRaw(IntPtr device, [Out] byte[] name, uint length);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetPowerUsage")] private static extern int GetPowerUsage(IntPtr device, out uint milliwatts);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMaxClockInfo")] private static extern int GetMaxClock(IntPtr device, int type, out uint mhz);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMinMaxClockOfPState")] private static extern int GetMinMaxOfPState(IntPtr device, int type, int pstate, out uint min, out uint max);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMemoryInfo")] private static extern int GetMemoryInfo(IntPtr device, out NvmlMemory memory);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetBAR1MemoryInfo")] private static extern int GetBar1MemoryInfo(IntPtr device, out NvmlMemory memory);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetCurrPcieLinkGeneration")] private static extern int GetCurrentPcieGeneration(IntPtr device, out uint generation);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMaxPcieLinkGeneration")] private static extern int GetMaximumPcieGeneration(IntPtr device, out uint generation);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetCurrPcieLinkWidth")] private static extern int GetCurrentPcieWidth(IntPtr device, out uint width);
    [DllImport(Dll, EntryPoint = "nvmlDeviceGetMaxPcieLinkWidth")] private static extern int GetMaximumPcieWidth(IntPtr device, out uint width);

    [StructLayout(LayoutKind.Sequential)]
    private struct NvmlMemory
    {
        public ulong Total;
        public ulong Free;
        public ulong Used;
    }

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
    public static string? LockGraphicsClocks(int gpuIndex, int minMhz, int maxMhz) =>
        Write(gpuIndex, $"nvmlDeviceSetGpuLockedClocks({minMhz},{maxMhz})",
              dev => SetLocked(dev, (uint)Math.Max(0, minMhz), (uint)Math.Max(0, maxMhz)));

    /// <summary>Hand the clock range back to the driver. Returns null on success, else the reason.</summary>
    public static string? ResetGraphicsClocks(int gpuIndex) =>
        Write(gpuIndex, "nvmlDeviceResetGpuLockedClocks", ResetLocked);

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
        uint mw = 0;
        return Read(gpuIndex, dev => GetPowerUsage(dev, out mw)) ? mw / 1000.0 : double.NaN;
    }

    /// <summary>
    /// Highest graphics clock the driver will report, in MHz; 0 when NVML cannot say. Used as the top
    /// of the clock-range control, so the slider stops where the card does.
    /// </summary>
    public static int MaxGraphicsClockMhz(int gpuIndex)
    {
        uint mhz = 0;
        return Read(gpuIndex, dev => GetMaxClock(dev, ClockGraphics, out mhz)) ? (int)mhz : 0;
    }

    /// <summary>
    /// Whether BAR1 spans nearly all VRAM. A legacy 256 MB aperture means disabled; when Resizable
    /// BAR is enabled NVML reports an aperture close to the frame-buffer size.
    /// </summary>
    public static bool? ResizableBarEnabled(int gpuIndex)
    {
        NvmlMemory vram = default, bar = default;
        bool ok = Read(gpuIndex, dev =>
        {
            int memoryResult = GetMemoryInfo(dev, out vram);
            return memoryResult == Success ? GetBar1MemoryInfo(dev, out bar) : memoryResult;
        });
        if (!ok || vram.Total == 0 || bar.Total == 0) return null;
        return bar.Total >= vram.Total * 0.9;
    }

    /// <summary>
    /// PCIe capability and the link negotiated right now. NVML reports both sides independently,
    /// which matters because the generation commonly drops while the GPU is idle even though lane
    /// width and the slot/card capability do not change.
    /// </summary>
    public static (int MaxGeneration, int MaxWidth, int CurrentGeneration, int CurrentWidth) PcieLink(int gpuIndex)
    {
        uint maxGeneration = 0, maxWidth = 0, currentGeneration = 0, currentWidth = 0;
        bool haveMaxGeneration = Read(gpuIndex, dev => GetMaximumPcieGeneration(dev, out maxGeneration));
        bool haveMaxWidth = Read(gpuIndex, dev => GetMaximumPcieWidth(dev, out maxWidth));
        bool haveCurrentGeneration = Read(gpuIndex, dev => GetCurrentPcieGeneration(dev, out currentGeneration));
        bool haveCurrentWidth = Read(gpuIndex, dev => GetCurrentPcieWidth(dev, out currentWidth));
        return (
            haveMaxGeneration ? (int)maxGeneration : 0,
            haveMaxWidth ? (int)maxWidth : 0,
            haveCurrentGeneration ? (int)currentGeneration : 0,
            haveCurrentWidth ? (int)currentWidth : 0);
    }

    /// <summary>
    /// Lowest graphics clock the driver will discuss, in MHz; 0 when it will not say.
    ///
    /// Asked per p-state and reduced to the smallest answer, because no single call reports it: a
    /// 5070 Ti says 495 MHz for P0 and 180 for P8, and refuses several p-states outright. 180 is the
    /// real floor, and the driver accepts it as one — tested, along with the 210 that used to be
    /// compiled in here as a guess.
    /// </summary>
    public static int MinGraphicsClockMhz(int gpuIndex)
    {
        if (!IsAvailable) return 0;
        lock (Gate)
        {
            try
            {
                if (GetHandle((uint)gpuIndex, out var dev) != Success) return 0;
                int best = 0;
                for (int pstate = 0; pstate <= 15; pstate++)
                {
                    if (GetMinMaxOfPState(dev, ClockGraphics, pstate, out uint lo, out _) != Success) continue;
                    if (lo > 0 && (best == 0 || lo < best)) best = (int)lo;
                }
                return best;
            }
            catch (Exception) { return 0; }
        }
    }

    /// <summary>
    /// Bumped every time the session is thrown away and reopened, which only happens when the
    /// display driver has been reset under us. Anything remembered about driver state from before
    /// that point — a clock lock, say — describes a card that no longer exists.
    /// </summary>
    public static int SessionEpoch { get; private set; }

    /// <summary>Device name as NVML sees it — used to confirm the index lines up with the NVAPI one.</summary>
    public static string? DeviceName(int gpuIndex)
    {
        var buf = new byte[96];
        if (!Read(gpuIndex, dev => GetNameRaw(dev, buf, (uint)buf.Length))) return null;
        int len = Array.IndexOf(buf, (byte)0);
        return System.Text.Encoding.UTF8.GetString(buf, 0, len < 0 ? buf.Length : len);
    }

    /// <summary>
    /// True when a return code means this process's session has gone stale rather than the request
    /// being wrong — the one kind of failure a re-initialise can fix.
    ///
    /// A display driver reset invalidates every NVML session open across it. NVML has a code that
    /// says exactly that, GPU_IS_LOST, but a 5070 Ti coming back from one answers with UNKNOWN
    /// instead, which is why 999 is in this list. It was measured rather than assumed: a game took
    /// the driver down (nvlddmkm event 14, then 153 three times), and from that moment
    /// nvmlDeviceResetGpuLockedClocks returned 999 for as long as the app stayed open — eleven
    /// times over three quarters of a minute — while every NVAPI write in the same process carried
    /// on succeeding. A card that answers one library and not the other is not a broken card; it is
    /// a session that did not survive the reset. A fresh process was fine, and shutting NVML down
    /// and bringing it back up is what turns this process into a fresh one.
    /// </summary>
    internal static bool SessionMayBeStale(int result) =>
        result is Uninitialized or GpuIsLost or ResetRequired or Unknown;

    /// <summary>
    /// Run a call that changes something, retrying once on a new session when the first failure
    /// looks stale. Null on success, else the reason.
    /// </summary>
    private static string? Write(int gpuIndex, string name, Func<IntPtr, int> call)
    {
        if (!IsAvailable) return LastError ?? "NVML unavailable";
        lock (Gate)
        {
            try
            {
                var (r, reached) = Attempt(gpuIndex, call);
                if (SessionMayBeStale(r) && Reestablish()) (r, reached) = Attempt(gpuIndex, call);
                if (r == Success) return null;

                string where = reached ? name : "nvmlDeviceGetHandleByIndex";
                return SessionMayBeStale(r)
                    ? $"{where}: {Describe(r)} — the display driver was reset out from under this " +
                      "session, which a game crashing will do, and a new session could not reach " +
                      "the card either. Restart Roch GPU."
                    : $"{where}: {Describe(r)}";
            }
            catch (Exception e) { return e.Message; }
        }
    }

    /// <summary>
    /// Run a call that reads something, recovering the same way. False when there is no answer, so
    /// each caller keeps its own way of saying "cannot say".
    /// </summary>
    private static bool Read(int gpuIndex, Func<IntPtr, int> call)
    {
        if (!IsAvailable) return false;
        lock (Gate)
        {
            try
            {
                int r = Attempt(gpuIndex, call).Result;
                if (SessionMayBeStale(r) && Reestablish()) r = Attempt(gpuIndex, call).Result;
                return r == Success;
            }
            catch (Exception) { return false; }
        }
    }

    /// <summary>Fetch the device and run the call. Reached says whether the call itself was made.</summary>
    private static (int Result, bool Reached) Attempt(int gpuIndex, Func<IntPtr, int> call)
    {
        int r = GetHandle((uint)gpuIndex, out var dev);
        return r == Success ? (call(dev), true) : (r, false);
    }

    /// <summary>
    /// Drop the session and open a new one. True when the new one initialised.
    ///
    /// Nothing rate-limits this because nothing needs to: a card that is genuinely gone fails the
    /// re-initialise too, IsAvailable then answers false, and every later call stops at the top of
    /// Read or Write without reaching here.
    /// </summary>
    private static bool Reestablish()
    {
        TryShutdown();
        if (!IsAvailable) return false;
        SessionEpoch++;
        return true;
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
