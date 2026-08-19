namespace GpuTuner.Core.Models;

/// <summary>
/// Translates one absolute "I want the core to run at N mV" target into the two levers NVIDIA
/// actually exposes, which pull in opposite directions:
///
///   above the stock curve top  → voltage boost %, raising the ceiling
///   below the stock curve top  → V/F curve flatten, so the card stops asking for more
///   exactly at the top         → neither; stock behaviour
///
/// Presenting them as one slider is honest because they're mutually exclusive: boosting the
/// ceiling while flattening the curve down is self-defeating, and nothing good comes of both.
/// </summary>
public static class VoltagePlan
{
    /// <param name="targetMv">Desired core voltage ceiling, absolute mV.</param>
    /// <param name="stockMaxMv">Highest voltage on the card's stock V/F curve.</param>
    /// <param name="maxMv">Highest reachable voltage, i.e. stock top plus full boost headroom.</param>
    /// <returns>Boost percent (0-100) and the negative mV curve offset (0 = no flatten).</returns>
    public static (int boostPercent, int curveOffsetMv) Compute(int targetMv, int stockMaxMv, int maxMv)
    {
        if (targetMv <= 0 || stockMaxMv <= 0) return (0, 0);        // "leave it alone"

        if (targetMv > stockMaxMv)
        {
            int headroom = maxMv - stockMaxMv;
            if (headroom <= 0) return (0, 0);                        // no boost available on this card
            int pct = (int)Math.Round((targetMv - stockMaxMv) * 100.0 / headroom);
            return (Math.Clamp(pct, 0, 100), 0);
        }

        if (targetMv == stockMaxMv) return (0, 0);
        return (0, targetMv - stockMaxMv);                           // negative → flatten above target
    }

    /// <summary>The voltage a given boost/offset pair corresponds to — used to seed the slider.</summary>
    public static int ToTargetMv(int boostPercent, int curveOffsetMv, int stockMaxMv, int maxMv)
    {
        if (stockMaxMv <= 0) return 0;
        if (curveOffsetMv < 0) return stockMaxMv + curveOffsetMv;
        if (boostPercent > 0) return stockMaxMv + (int)Math.Round(boostPercent / 100.0 * (maxMv - stockMaxMv));
        return stockMaxMv;
    }

    /// <summary>
    /// The highest voltage the card can actually select right now: its measured roof with any boost
    /// applied, brought down by a flattened curve when that sits lower.
    ///
    /// The two inputs are measured against <em>different baselines</em>, and mixing them is how this
    /// went wrong once already. <paramref name="stockCeilingMv"/> is what the card has been observed
    /// to reach under load. <paramref name="curveOffsetMv"/> is measured against
    /// <paramref name="curveTopMv"/>, the top of the V/F table, which runs a couple of hundred
    /// millivolts above anything the card will ever select. Subtracting one from the other gave
    /// 1035 − (1000 − 1240) = 795 and drew "card tops out at 795 mV" beside a "capped 1000 mV"
    /// marker on the same graph — two labels for one setting, disagreeing.
    ///
    /// So convert the offset back to an absolute voltage against its own baseline first, then take
    /// whichever roof is lower. Taking the minimum is also what makes this safe against the curve
    /// reader occasionally returning the shorter table: an over-estimated plateau is discarded by
    /// the measured ceiling rather than believed.
    /// </summary>
    public static int CeilingMv(int boostPercent, int curveOffsetMv, int stockCeilingMv, int maxMv, int curveTopMv)
    {
        int ceiling = ToTargetMv(boostPercent, 0, stockCeilingMv, maxMv);
        if (curveOffsetMv < 0 && curveTopMv > 0)
            ceiling = Math.Min(ceiling, curveTopMv + curveOffsetMv);
        return ceiling;
    }
}
