namespace GpuTuner.Core.Models;

/// <summary>
/// The levers behind their own Enable/Disable button in the XOC window. Each one is armed
/// separately, because they fail in unrelated ways: a rail ceiling that browns the card out says
/// nothing about whether a crossbar offset is stable, and being forced to arm both to test either
/// is how a session ends up unable to say which of two changes hung it.
/// </summary>
[Flags]
public enum XocLever
{
    None = 0,
    Nvvdd = 1,
    Msvdd = 2,
    Xbar = 4,
    Sys = 8,
    Video = 16,
    ClockRange = 32,
    All = Nvvdd | Msvdd | Xbar | Sys | Video | ClockRange
}

public static class XocLeverExtensions
{
    public static bool Has(this XocLever set, XocLever one) => (set & one) == one;
    public static XocLever With(this XocLever set, XocLever one, bool on) => on ? set | one : set & ~one;
}
