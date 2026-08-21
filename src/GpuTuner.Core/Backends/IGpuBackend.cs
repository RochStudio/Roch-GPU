using GpuTuner.Core.Models;

namespace GpuTuner.Core.Backends;

/// <summary>
/// Vendor-neutral control surface. One implementation per vendor API (NVAPI today, ADLX later)
/// plus a mock for running the UI without hardware.
/// </summary>
public interface IGpuBackend : IDisposable
{
    string BackendName { get; }

    /// <summary>Initialise the vendor API and enumerate devices. Throws if the driver/API isn't available.</summary>
    void Initialize();

    IReadOnlyList<GpuDevice> Devices { get; }

    GpuCapabilities GetCapabilities(int gpuIndex);
    GpuTelemetry ReadTelemetry(int gpuIndex);

    /// <summary>
    /// Temperature-only read, for ticks where nothing is on screen and the sample exists purely to
    /// step a fan curve. A full <see cref="ReadTelemetry"/> costs ~13 ms of synchronous driver calls
    /// on NVIDIA, most of it in the power-topology query; running that every second behind a game is
    /// exactly what this avoids. Only <see cref="GpuTelemetry.TemperatureC"/> is guaranteed to be
    /// populated — never store the result or show it.
    ///
    /// The default falls back to the full read, which is correct but not cheap; a backend overrides
    /// it when it has a cheaper path to temperature.
    /// </summary>
    GpuTelemetry ReadTemperatureOnly(int gpuIndex) => ReadTelemetry(gpuIndex);

    GpuTuningState ReadTuningState(int gpuIndex);

    void SetCoreOffset(int gpuIndex, int offsetMhz);
    void SetMemoryOffset(int gpuIndex, int offsetMhz);
    void SetPowerLimit(int gpuIndex, int percent);
    void SetTempLimit(int gpuIndex, int celsius);

    /// <summary>Core voltage boost in percent (0 = stock). Raises the ceiling of the V/F curve.</summary>
    void SetVoltageBoost(int gpuIndex, int percent);

    /// <summary>
    /// Negative mV offset: caps the V/F curve that far below its stock top. 0 clears the edit.
    /// <paramref name="extraClockMhz"/> lifts the clock at the cap point — pass the user's core offset here,
    /// because on Pascal and later the pstate offset and the curve delta table are the same storage, so
    /// writing both would clobber the flatten.
    /// </summary>
    void SetVoltageCurveOffset(int gpuIndex, int offsetMv, int extraClockMhz = 0);

    /// <summary>
    /// Cap the core at an absolute voltage in mV; 0 clears it. Absolute rather than an offset so the
    /// cap doesn't depend on whether "stock" means the curve's last point or the VID the card really
    /// reaches — those differ by a step (1090 vs 1100 mV on a 4070 Ti).
    /// </summary>
    void SetVoltageLock(int gpuIndex, int targetMv) { }

    /// <summary>
    /// The absolute voltage the core is currently locked to, in mV; 0 = no lock.
    /// Returns -1 when the backend cannot report it, so callers can tell "no lock" from "no answer".
    /// </summary>
    int ReadVoltageLockMv(int gpuIndex) => -1;

    /// <summary>
    /// Which lever is currently holding the voltage cap ("none" when uncapped). Backends may have
    /// more than one way to do it, and when one is silently ignored the user should be told which
    /// one actually took effect.
    /// </summary>
    string VoltageLockMechanism => "n/a";

    /// <summary>
    /// Fans stop completely below the driver's idle threshold (AMD "zero RPM"). Backends that do
    /// not expose the toggle ignore it.
    /// </summary>
    void SetZeroRpm(int gpuIndex, bool enabled) { }

    /// <summary>
    /// Select a memory timing preset by index into <see cref="GpuCapabilities.MemoryTimingOptions"/>.
    /// </summary>
    void SetMemoryTiming(int gpuIndex, int level) { }

    /// <summary>
    /// Hand a fan curve to the driver, for cards whose curve runs in hardware
    /// (<see cref="GpuCapabilities.FanCurveIsHardware"/>). Backends without one use the polling
    /// loop in TuningService instead.
    /// </summary>
    void SetFanCurve(int gpuIndex, FanCurve curve) =>
        throw new GpuBackendException("This backend has no hardware fan curve.");

    /// <summary>
    /// Move the core power rail's own voltage ceiling to an absolute mV value; 0 restores the
    /// card's default. Distinct from the boost percentage, which asks for headroom within the
    /// existing ceiling rather than moving it. Backends without the lever ignore this.
    /// </summary>
    void SetVoltageRailMax(int gpuIndex, int millivolts) { }

    /// <summary>
    /// Move the MSVDD rail's ceiling to an absolute mV value; 0 restores the default. MSVDD feeds
    /// the crossbar, SYS and video domains rather than the shader core.
    /// </summary>
    void SetMsvddRailMax(int gpuIndex, int millivolts) { }

    /// <summary>
    /// Move a voltage rail's floor — the lowest it may drop to — to an absolute mV value; 0 restores
    /// the card's own default.
    /// </summary>
    void SetVoltageRailFloor(int gpuIndex, int millivolts) { }
    void SetMsvddRailFloor(int gpuIndex, int millivolts) { }

    /// <summary>
    /// Offset the crossbar (interconnect) clock, in MHz. Backends without the domain ignore this.
    /// </summary>
    void SetXbarOffset(int gpuIndex, int offsetMhz) { }

    /// <summary>
    /// Offset the SYS and video clocks, in MHz. Same private family as the crossbar; backends without
    /// it ignore these.
    /// </summary>
    void SetSysOffset(int gpuIndex, int offsetMhz) { }
    void SetVideoOffset(int gpuIndex, int offsetMhz) { }

    /// <summary>Force a fan to a fixed duty. fanIndex = -1 means all fans.</summary>
    void SetFanSpeed(int gpuIndex, int fanIndex, int percent);

    /// <summary>Return fan control to the driver/VBIOS.</summary>
    void SetFanAuto(int gpuIndex);

    /// <summary>Restore all tunables to their driver defaults.</summary>
    void ResetToDefaults(int gpuIndex);

    /// <summary>Raw dump of what the vendor API reports, for debugging unsupported cards.</summary>
    string GetDiagnostics(int gpuIndex) => "No diagnostics for this backend.";

    /// <summary>Read the editable V/F curve points (valid points only). Empty if unsupported.</summary>
    IReadOnlyList<VfCurveSample> ReadVfCurve(int gpuIndex) => Array.Empty<VfCurveSample>();

    /// <summary>
    /// Write explicit per-point target frequencies. Each sample's Index + LiveMhz (used as the target)
    /// define the point; every other point is returned to stock. Pass an empty list to clear the curve.
    /// </summary>
    void SetVfCurveTargets(int gpuIndex, IReadOnlyList<VfCurveSample> targets) =>
        throw new GpuBackendException("V/F curve editing is not supported by this backend.");
}

public sealed class GpuBackendException : Exception
{
    public GpuBackendException(string message, Exception? inner = null) : base(message, inner) { }
}
