using System.Runtime.InteropServices;
using GpuTuner.Core.Models;

namespace GpuTuner.Core.Backends.Amd;

/// <summary>Read-only ADLX performance metrics, loaded from the installed AMD driver.</summary>
internal sealed class AdlxTelemetry : IDisposable
{
    private readonly object _gate = new();
    private IntPtr _system, _mapping, _performance;
    private bool _attempted, _ownsRuntime, _disposed;
    public string Status { get; private set; } = "ADLX not sampled";

    [DllImport("amdadlx64.dll", CallingConvention = CallingConvention.Cdecl)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static extern int ADLXQueryFullVersion(out ulong version);
    [DllImport("amdadlx64.dll", CallingConvention = CallingConvention.Cdecl)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static extern int ADLXInitializeWithCallerAdl(ulong version, out IntPtr system, out IntPtr mapping, IntPtr context, FreeMemory free);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void FreeMemory(IntPtr buffer);
    private static readonly FreeMemory Free = buffer =>
    {
        if (buffer == IntPtr.Zero) return;
        Marshal.FreeCoTaskMem(Marshal.ReadIntPtr(buffer));
        Marshal.WriteIntPtr(buffer, IntPtr.Zero);
    };
    [DllImport("amdadlx64.dll", CallingConvention = CallingConvention.Cdecl)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static extern int ADLXTerminate();

    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetObject(IntPtr self, out IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int MapGpu(IntPtr self, int adapter, out IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GpuObject(IntPtr self, IntPtr gpu, out IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int Query(IntPtr self, [MarshalAs(UnmanagedType.LPWStr)] string iid, out IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetInt(IntPtr self, out int value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetDouble(IntPtr self, out double value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetBool(IntPtr self, out byte value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ReleaseObject(IntPtr self);

    private static T Slot<T>(IntPtr obj, int index) where T : Delegate =>
        Marshal.GetDelegateForFunctionPointer<T>(Marshal.ReadIntPtr(Marshal.ReadIntPtr(obj), index * IntPtr.Size));
    private static void Release(IntPtr obj) { if (obj != IntPtr.Zero) Slot<ReleaseObject>(obj, 1)(obj); }
    private static bool Ok(int rc) => rc is >= 0 and <= 2;

    // Slots and types follow AMD's IPerformanceMonitoring[2,3].h C vtables.
    internal sealed record Metric(string Key, string Label, string Unit, int ValueSlot, int SupportSlot, bool Integer, int Revision = 0);
    internal static readonly Metric[] Metrics =
    {
        new("usage", "GPU utilization", "%", 4, 3, false),
        new("core", "GPU clock", "MHz", 5, 4, true),
        new("memoryclock", "Memory clock", "MHz", 6, 5, true),
        new("temperature", "GPU temperature", "°C", 7, 6, false),
        new("hotspot", "Hot spot", "°C", 8, 7, false),
        new("power", "GPU power (ADLX)", "W", 9, 8, false),
        new("board", "Board power", "W", 10, 9, false),
        new("fan", "Fan speed", "RPM", 11, 10, true),
        new("vram", "Dedicated VRAM used", "MB", 12, 11, true),
        new("voltage", "GPU voltage", "mV", 13, 12, true),
        new("intake", "Intake temperature", "°C", 14, 24, false),
        new("memorytemp", "Memory junction", "°C", 15, 25, false, 1),
        new("shared", "Shared GPU memory used", "MB", 18, 31, true, 2),
        new("fanduty", "Fan duty", "%", 19, 33, true, 3)
    };

    public IReadOnlyDictionary<string, double> Read(IntPtr context, int adapter)
    {
        lock (_gate)
        {
            var readings = new Dictionary<string, double>();
            if (_disposed) return readings;
            try
            {
                if (!_attempted)
                {
                    _attempted = true;
                    int rc = ADLXQueryFullVersion(out ulong version);
                    if (!Ok(rc)) { Status = $"ADLX version query: {rc}"; return readings; }
                    rc = ADLXInitializeWithCallerAdl(version, out _system, out _mapping, context, Free);
                    _ownsRuntime = rc == 0;
                    if (!Ok(rc) || _system == IntPtr.Zero || _mapping == IntPtr.Zero)
                    { Status = $"ADLX initialization: {rc}"; return readings; }
                    rc = Slot<GetObject>(_system, 9)(_system, out _performance);
                    Status = $"ADLX performance service: {rc}";
                    if (!Ok(rc)) _performance = IntPtr.Zero;
                }
                if (_performance == IntPtr.Zero) return readings;
                IntPtr gpu = IntPtr.Zero, metrics = IntPtr.Zero, support = IntPtr.Zero;
                var acquired = new List<IntPtr>();
                try
                {
                    if (!Ok(Slot<MapGpu>(_mapping, 1)(_mapping, adapter, out gpu)) || gpu == IntPtr.Zero)
                    { Status = "ADLX adapter mapping unavailable"; return readings; }
                    if (!Ok(Slot<GpuObject>(_performance, 21)(_performance, gpu, out support)) || support == IntPtr.Zero)
                    { Status = "ADLX support query unavailable"; return readings; }
                    if (!Ok(Slot<GpuObject>(_performance, 18)(_performance, gpu, out metrics)) || metrics == IntPtr.Zero)
                    { Status = "ADLX current metrics unavailable"; return readings; }
                    var metricInterfaces = new Dictionary<int, IntPtr> { [0] = metrics };
                    var supportInterfaces = new Dictionary<int, IntPtr> { [0] = support };
                    for (int rev = 1; rev <= 3; rev++)
                    {
                        if (Ok(Slot<Query>(metrics, 2)(metrics, $"IADLXGPUMetrics{rev}", out var m)) && m != IntPtr.Zero)
                        { metricInterfaces[rev] = m; acquired.Add(m); }
                        if (Ok(Slot<Query>(support, 2)(support, $"IADLXGPUMetricsSupport{rev}", out var s)) && s != IntPtr.Zero)
                        { supportInterfaces[rev] = s; acquired.Add(s); }
                    }
                    foreach (var d in Metrics)
                    {
                        if (!metricInterfaces.TryGetValue(d.Revision, out var m) || !supportInterfaces.TryGetValue(d.Revision, out var s)) continue;
                        if (!Ok(Slot<GetBool>(s, d.SupportSlot)(s, out byte enabled)) || enabled == 0) continue;
                        double value;
                        int rc;
                        if (d.Integer) { rc = Slot<GetInt>(m, d.ValueSlot)(m, out int v); value = v; }
                        else rc = Slot<GetDouble>(m, d.ValueSlot)(m, out value);
                        if (Ok(rc) && double.IsFinite(value) && value >= 0) readings[d.Key] = value;
                    }
                    Status = $"ADLX: {readings.Count} live metrics";
                    return readings;
                }
                finally
                {
                    foreach (var ptr in acquired) Release(ptr);
                    Release(metrics); Release(support); Release(gpu);
                }
            }
            catch (DllNotFoundException) { Status = "ADLX library not installed"; }
            catch (EntryPointNotFoundException) { Status = "ADLX driver interface unavailable"; }
            return readings;
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed) return;
            _disposed = true;
            Release(_performance); _performance = IntPtr.Zero;
            // IADLXSystem and IADLMapping are runtime-owned (no Acquire/Release slots).
            if (_ownsRuntime) ADLXTerminate();
            _system = _mapping = IntPtr.Zero;
        }
    }
}
