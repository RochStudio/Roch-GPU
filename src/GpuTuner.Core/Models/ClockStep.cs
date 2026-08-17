namespace GpuTuner.Core.Models;

/// <summary>
/// Granularity the driver actually applies clock offsets at.
///
/// NVIDIA quantises the core offset to 15 MHz — every clock in the V/F table is a multiple of it —
/// so a finer number is rounded away somewhere between the slider and the card, and the value you
/// typed is not the value you get. Snapping in the UI keeps those two the same thing.
/// </summary>
public static class ClockStep
{
    /// <summary>Core clock offset granularity, MHz.</summary>
    public const int CoreMhz = 15;

    /// <summary>
    /// Nearest multiple of <paramref name="step"/>, anchored on zero. Anchoring matters: snapping to a
    /// grid that starts at the slider's minimum (-1000 MHz, not a multiple of 15) would put stock out
    /// of reach. Halfway rounds away from zero so -22 and +22 land symmetrically.
    /// </summary>
    public static int Snap(int value, int step) =>
        step <= 1 ? value : (int)System.Math.Round(value / (double)step, System.MidpointRounding.AwayFromZero) * step;

    /// <summary>
    /// Snap, then keep the result inside [min, max] <em>without</em> falling off the grid. Clamping
    /// after snapping is not enough: the driver's limits are rarely multiples of the step (±1000 MHz
    /// is not a multiple of 15), so the plain clamp hands back an off-grid endpoint. Stepping back
    /// toward zero instead gives the nearest in-range value that is still a multiple of the step.
    /// </summary>
    public static int SnapWithin(int value, int step, int min, int max)
    {
        if (min > max) return value;
        int snapped = Snap(System.Math.Clamp(value, min, max), step);
        if (step <= 1) return System.Math.Clamp(snapped, min, max);
        if (snapped > max) snapped = max / step * step;         // integer division truncates toward zero
        if (snapped < min) snapped = -(-min / step * step);
        return System.Math.Clamp(snapped, min, max);
    }
}
