namespace GpuTuner.Core.Backends.Nvidia;

/// <summary>One point of the voltage/frequency curve.</summary>
public readonly record struct VfPoint(uint VoltageUv, int FrequencyKhz);

/// <summary>
/// Pure V/F curve maths — no NVAPI, so it can be unit-tested without a GPU.
///
/// NVIDIA exposes no voltage knob. The only way to undervolt is to edit the V/F curve:
/// pick a target voltage, then clamp every point ABOVE it down to the frequency at that point.
/// Once higher voltages buy no extra clock, the boost algorithm stops asking for them — that's
/// the undervolt. It's exactly what Afterburner's Ctrl+F does, done arithmetically.
/// </summary>
public static class VfCurve
{
    /// <summary>
    /// Compute the per-point frequency deltas (kHz) that flatten the curve at <paramref name="capVoltageUv"/>.
    ///
    /// Points below the cap keep their stock frequency (delta 0). The point at the cap is lifted by
    /// <paramref name="extraClockKhz"/>. Every point above the cap is clamped to that same frequency,
    /// which yields negative deltas — the actual undervolt.
    /// </summary>
    /// <param name="basePoints">The STOCK curve (current curve minus currently-applied deltas).</param>
    /// <param name="capVoltageUv">Voltage ceiling in microvolts.</param>
    /// <param name="extraClockKhz">Extra clock to request at the cap point (0 = pure undervolt).</param>
    /// <param name="rangeMinKhz">Driver's minimum allowed delta.</param>
    /// <param name="rangeMaxKhz">Driver's maximum allowed delta.</param>
    public static int[] ComputeFlattenDeltas(
        IReadOnlyList<VfPoint> basePoints,
        uint capVoltageUv,
        int extraClockKhz = 0,
        int rangeMinKhz = int.MinValue,
        int rangeMaxKhz = int.MaxValue)
    {
        var deltas = new int[basePoints.Count];
        if (basePoints.Count == 0 || capVoltageUv == 0) return deltas;

        // Index of the highest-voltage valid point at or below the cap.
        int t = -1;
        for (int i = 0; i < basePoints.Count; i++)
        {
            var p = basePoints[i];
            if (!IsValid(p)) continue;
            if (p.VoltageUv <= capVoltageUv && (t < 0 || p.VoltageUv > basePoints[t].VoltageUv)) t = i;
        }

        // Cap is below the entire curve: clamp everything to the lowest valid point instead of
        // returning garbage. Better to underclock predictably than to write nonsense.
        if (t < 0)
        {
            for (int i = 0; i < basePoints.Count; i++)
                if (IsValid(basePoints[i]) && (t < 0 || basePoints[i].VoltageUv < basePoints[t].VoltageUv)) t = i;
            if (t < 0) return deltas;
        }

        int targetFreq = basePoints[t].FrequencyKhz + extraClockKhz;
        uint tVolt = basePoints[t].VoltageUv;

        for (int i = 0; i < basePoints.Count; i++)
        {
            var p = basePoints[i];
            if (!IsValid(p)) { deltas[i] = 0; continue; }
            if (p.VoltageUv < tVolt) { deltas[i] = 0; continue; }   // below the cap: leave stock
            deltas[i] = Math.Clamp(targetFreq - p.FrequencyKhz, rangeMinKhz, rangeMaxKhz);
        }
        return deltas;
    }

    /// <summary>
    /// Look at an effective curve and work out whether it's been flattened, and where.
    /// Returns the lowest voltage of the top plateau, or 0 if the curve still rises to the top.
    /// Used to report the real state back to the UI rather than echoing what we asked for.
    /// </summary>
    public static uint InferCapVoltageUv(IReadOnlyList<VfPoint> effectivePoints)
    {
        var pts = effectivePoints.Where(IsValid).OrderBy(p => p.VoltageUv).ToList();
        if (pts.Count < 5) return 0;

        int topFreq = pts[^1].FrequencyKhz;
        int i = pts.Count - 1;
        while (i > 0 && Math.Abs(pts[i - 1].FrequencyKhz - topFreq) <= 1000) i--;

        // Stock curves flatten out naturally at the top — a 4070 Ti has two adjacent points at
        // 2820 MHz with no edit applied. Only call it a flatten when the plateau is long enough
        // and wide enough in volts that it cannot be the curve's own shape.
        int plateauPoints = pts.Count - i;
        int plateauMv = (int)((pts[^1].VoltageUv - pts[i].VoltageUv) / 1000);
        if (plateauPoints < 4 || plateauMv < 25) return 0;
        return pts[i].VoltageUv;
    }

    /// <summary>Highest voltage on the curve — the stock ceiling a negative offset is measured from.</summary>
    public static uint MaxVoltageUv(IReadOnlyList<VfPoint> points)
    {
        uint max = 0;
        foreach (var p in points) if (IsValid(p) && p.VoltageUv > max) max = p.VoltageUv;
        return max;
    }

    private static bool IsValid(VfPoint p) => p.VoltageUv > 0 && p.FrequencyKhz > 0;

    /// <summary>
    /// Per-point deltas to drive each edited point to an explicit target frequency (graphical editor).
    /// <paramref name="targetKhzByIndex"/> maps a base-point index to the frequency the user dragged it to;
    /// indices absent from the map keep stock (delta 0). Deltas are clamped to the driver's range.
    /// </summary>
    public static int[] ComputeTargetDeltas(
        IReadOnlyList<VfPoint> basePoints,
        IReadOnlyDictionary<int, int> targetKhzByIndex,
        int rangeMinKhz = int.MinValue,
        int rangeMaxKhz = int.MaxValue)
    {
        var deltas = new int[basePoints.Count];
        foreach (var kv in targetKhzByIndex)
        {
            int i = kv.Key;
            if (i < 0 || i >= basePoints.Count) continue;
            if (!IsValid(basePoints[i])) continue;
            deltas[i] = Math.Clamp(kv.Value - basePoints[i].FrequencyKhz, rangeMinKhz, rangeMaxKhz);
        }
        return deltas;
    }
}
