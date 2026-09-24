using System.Runtime.InteropServices;

namespace GpuTuner.Core.Backends.Intel;

// Windows x64 IGCL ABI, from Intel's public igcl_api.h. Load only the installed
// system driver library, never a DLL from the working directory.
internal sealed class Igcl : IDisposable
{
    private readonly IntPtr _library;
    private readonly Dictionary<(string, Type), Delegate> _exports = new();
    internal Igcl()
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess)
            throw new GpuBackendException("Intel IGCL requires 64-bit Windows.");
        _library = NativeLibrary.Load(Path.Combine(Environment.SystemDirectory, "ControlLib.dll"));
    }
    internal T Function<T>(string name) where T : Delegate
    {
        var key = (name, typeof(T));
        if (!_exports.TryGetValue(key, out var function))
            _exports[key] = function = Marshal.GetDelegateForFunctionPointer<T>(NativeLibrary.GetExport(_library, name));
        return (T)function;
    }
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint Init([In, Out] byte[] args, out IntPtr handle);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint Handle(IntPtr handle);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint Buffer(IntPtr handle, [In, Out] byte[] data);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint PointerBuffer(IntPtr handle, IntPtr data);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint Enumerate(IntPtr handle, ref uint count, [Out] IntPtr[]? handles);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint GetDouble(IntPtr handle, out double value);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint SetDouble(IntPtr handle, double value);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint ReadCurve(IntPtr handle, int type, int detail, ref uint count, [Out] Point[]? points);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] internal delegate uint WriteCurve(IntPtr handle, uint count, [In] Point[] points);
    [StructLayout(LayoutKind.Sequential)] internal struct Point { public uint Voltage, Frequency; }
    internal static byte[] Data(int size, byte version = 0)
    {
        var b = new byte[size]; Put(b, 0, size); b[4] = version; return b;
    }
    internal static void Put(byte[] b, int at, int value) => BitConverter.GetBytes(value).CopyTo(b, at);
    internal static int Int(byte[] b, int at) => BitConverter.ToInt32(b, at);
    internal static void Check(uint result, string operation)
    {
        if (result != 0) throw new GpuBackendException($"Intel {operation} failed (IGCL 0x{result:X8}).");
    }
    internal IntPtr[] Handles(string function, IntPtr parent)
    {
        var f = Function<Enumerate>(function); uint count = 0;
        Check(f(parent, ref count, null), function);
        if (count > 128) throw new GpuBackendException("Intel returned an invalid handle count.");
        if (count == 0) return Array.Empty<IntPtr>();
        var handles = new IntPtr[count];
        Check(f(parent, ref count, handles), function);
        if (count > handles.Length) throw new GpuBackendException("Intel handle count changed; reopen the app.");
        return handles.Take((int)count).ToArray();
    }
    internal static double Sensor(byte[] b, int at)
    {
        if (at < 0 || at + 24 > b.Length || b[at] == 0) return double.NaN;
        double value = Int(b, at + 8) switch
        {
            0 => (sbyte)b[at + 16], 1 => b[at + 16],
            2 => BitConverter.ToInt16(b, at + 16), 3 => BitConverter.ToUInt16(b, at + 16),
            4 => Int(b, at + 16), 5 => BitConverter.ToUInt32(b, at + 16),
            6 => BitConverter.ToInt64(b, at + 16), 7 => BitConverter.ToUInt64(b, at + 16),
            8 => BitConverter.ToSingle(b, at + 16), 9 => BitConverter.ToDouble(b, at + 16),
            _ => double.NaN
        };
        return double.IsFinite(value) ? value : double.NaN;
    }
    public void Dispose() => NativeLibrary.Free(_library);
}
