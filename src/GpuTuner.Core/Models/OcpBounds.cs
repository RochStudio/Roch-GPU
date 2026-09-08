namespace GpuTuner.Core.Models;

/// <summary>
/// The window an OCP current limit may be moved within, in whole amps.
///
/// Half the stock figure at the bottom, half again above it at the top. Not a number invented here:
/// it is the bound HYDRA's own implementation applies before it writes, read out of its NVAPI.dll,
/// and the driver refuses anything outside its own window regardless — so a slider offering more
/// would only be offering values that come back as errors.
///
/// Amps rather than the driver's milliamps because nothing here is tunable to a milliamp: the card
/// quotes 300 A and 120 A flat, and a control offering three more digits of precision than the
/// hardware has would be inventing them.
/// </summary>
public static class OcpBounds
{
    public static int MinA(int stockMilliamps) => stockMilliamps > 0 ? stockMilliamps / 2000 : 0;
    public static int MaxA(int stockMilliamps) => stockMilliamps > 0 ? stockMilliamps * 3 / 2000 : 0;
}
