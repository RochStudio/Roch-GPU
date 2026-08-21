namespace GpuTuner.Core.Models;

/// <summary>
/// Current, minimum, maximum and running mean for one sensor.
///
/// The mean is kept as a running total rather than over a window, so it describes the whole session
/// since the last reset — which is the figure worth having when the question is "what did this card
/// do while I was playing", and the reason a monitoring tool shows four columns rather than one.
///
/// A reading of NaN is a sensor the card does not expose. Those are dropped rather than folded in as
/// zero, because a zero minimum on a temperature that was never read is a lie the table would then
/// display for the rest of the session.
/// </summary>
public sealed class SensorStat
{
    public double Current { get; private set; } = double.NaN;
    public double Minimum { get; private set; } = double.NaN;
    public double Maximum { get; private set; } = double.NaN;

    private double _total;
    private long _count;

    /// <summary>Mean of every reading since the last reset; NaN before the first one.</summary>
    public double Average => _count > 0 ? _total / _count : double.NaN;

    /// <summary>True once at least one real reading has arrived.</summary>
    public bool HasData => _count > 0;

    public void Add(double value)
    {
        if (double.IsNaN(value) || double.IsInfinity(value)) return;
        Current = value;
        if (double.IsNaN(Minimum) || value < Minimum) Minimum = value;
        if (double.IsNaN(Maximum) || value > Maximum) Maximum = value;
        _total += value;
        _count++;
    }

    public void Reset()
    {
        Current = Minimum = Maximum = double.NaN;
        _total = 0;
        _count = 0;
    }
}
