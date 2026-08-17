using GpuTuner.Core.Models;

namespace GpuTuner.Core.Services;

/// <summary>Fixed-capacity ring buffer of telemetry samples for the graphs.</summary>
public sealed class TelemetryHistory
{
    private readonly object _lock = new();
    private readonly GpuTelemetry[] _buf;
    private int _head, _count;

    public int Capacity => _buf.Length;

    public TelemetryHistory(int capacity)
    {
        _buf = new GpuTelemetry[Math.Max(2, capacity)];
    }

    public void Add(GpuTelemetry t)
    {
        lock (_lock)
        {
            _buf[_head] = t;
            _head = (_head + 1) % _buf.Length;
            if (_count < _buf.Length) _count++;
        }
    }

    public void Clear() { lock (_lock) { _head = 0; _count = 0; } }

    /// <summary>Oldest → newest snapshot.</summary>
    public GpuTelemetry[] Snapshot()
    {
        lock (_lock)
        {
            var arr = new GpuTelemetry[_count];
            int start = (_head - _count + _buf.Length) % _buf.Length;
            for (int i = 0; i < _count; i++) arr[i] = _buf[(start + i) % _buf.Length];
            return arr;
        }
    }

    /// <summary>Convenience: a single series projected out of the history.</summary>
    public double[] Series(Func<GpuTelemetry, double> selector) => Snapshot().Select(selector).ToArray();
}
