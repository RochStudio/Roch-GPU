using GpuTuner.Core.Backends;
using GpuTuner.Core.Models;

namespace GpuTuner.Core.Services;

/// <summary>
/// The "engine": owns a backend, applies profiles, runs the polling loop and the fan-curve controller.
/// UI-agnostic so it can be driven by WPF, a CLI (--apply-profile at startup) or tests.
/// </summary>
public sealed class TuningService : IDisposable
{
    private readonly object _lock = new();
    private CancellationTokenSource? _pollCts;
    private Task? _pollTask;

    public IGpuBackend Backend { get; }
    public int GpuIndex { get; private set; }
    public GpuCapabilities Capabilities { get; private set; } = new();
    public GpuDevice Device => Backend.Devices[GpuIndex];

    /// <summary>The profile currently applied to hardware (null = untouched since launch).</summary>
    public TuningProfile? AppliedProfile { get; private set; }

    /// <summary>Latest telemetry sample.</summary>
    public GpuTelemetry? Latest { get; private set; }

    /// <summary>
    /// Highest core voltage actually observed. The V/F curve's last point isn't the ceiling — the
    /// card selects a VID a step above it (1090 mV curve top, 1100 mV in practice), and that higher
    /// figure is what monitoring tools report, so it's what the UI should call "stock".
    /// </summary>
    public int ObservedMaxVoltageMv { get; private set; }

    /// <summary>How far above the curve's top point a reading can be and still count as "stock".</summary>
    private const int StockVidToleranceMv = 25;

    /// <summary>
    /// Stock ceiling: the curve's top point, nudged up to the VID the card really uses.
    ///
    /// Only readings within one V/F step of the curve top are trusted. Anything higher was reached
    /// *because* a boost was applied, and folding that back in would raise the ceiling to the top of
    /// the slider — after which nothing is above stock, so Apply would compute neither a boost nor a
    /// cap and write nothing while still reporting success.
    /// </summary>
    public int StockCeilingMv
    {
        get
        {
            int curveTop = Capabilities.StockMaxVoltageMv;
            if (curveTop <= 0) return ObservedMaxVoltageMv > 0 ? ObservedMaxVoltageMv : Capabilities.MaxVoltageMv;
            return Math.Clamp(ObservedMaxVoltageMv, curveTop, curveTop + StockVidToleranceMv);
        }
    }
    /// <summary>
    /// Feed a live voltage reading into the ceiling estimate. Only samples taken while the card is
    /// running its own voltage count: once we've boosted or capped it, what it reports says nothing
    /// about where stock sits.
    /// </summary>
    public void NoteVoltageObservation(double mv)
    {
        if (!VoltageIsUntouched || double.IsNaN(mv) || mv >= 1400) return;
        int curveTop = Capabilities.StockMaxVoltageMv;
        // A reading more than one V/F step above the curve's top point is boost, not stock — keeping
        // it would let a single sample poison the ceiling for the rest of the session.
        if (curveTop > 0 && mv > curveTop + StockVidToleranceMv) return;
        if (mv > ObservedMaxVoltageMv) ObservedMaxVoltageMv = (int)Math.Round(mv);
    }

    /// <summary>True while no voltage lever of ours is engaged, so live voltage still reflects stock.</summary>
    private bool VoltageIsUntouched
    {
        get
        {
            var a = AppliedProfile;
            if (a == null) return true;
            if (a.VoltageBoostPercent != 0 || a.VoltageOffsetMv != 0) return false;
            return a.TargetVoltageMv <= 0 || a.TargetVoltageMv >= Capabilities.StockMaxVoltageMv;
        }
    }

    /// <summary>Which lever last held the voltage cap, for the status line.</summary>
    public string VoltageLockMechanism { get; private set; } = "none";

    public TelemetryHistory History { get; }

    /// <summary>Raised on the polling thread after each sample. UI must marshal to its own thread.</summary>
    public event Action<GpuTelemetry>? TelemetryUpdated;
    public event Action<string>? Log;

    private FanCurve? _activeCurve;
    private double _lastCurveFanSent = double.NaN;

    /// <summary>
    /// True when the graphical curve editor has written per-point deltas. The delta table is the same
    /// storage the voltage-offset slider and the Pascal+ core-offset path use, so applying a profile
    /// will overwrite those edits — Apply says so rather than silently wiping them.
    /// </summary>
    public bool ManualCurveActive { get; private set; }

    public TuningService(IGpuBackend backend, int historySeconds = 120)
    {
        Backend = backend;
        History = new TelemetryHistory(historySeconds);
    }

    public void Initialize(int gpuIndex = 0)
    {
        Backend.Initialize();
        SelectGpu(gpuIndex);
    }

    public void SelectGpu(int index)
    {
        lock (_lock)
        {
            GpuIndex = index;
            Capabilities = Backend.GetCapabilities(index);
            History.Clear();
            Log?.Invoke($"Selected {Device.Name} via {Backend.BackendName}");
        }
    }

    // ------------------------------------------------------------------ apply

    /// <summary>Apply every setting in the profile. Returns a list of per-setting failures (empty on success).</summary>
    public IReadOnlyList<string> Apply(TuningProfile profile)
    {
        var errors = new List<string>();
        var p = profile.Clone();
        p.ClampTo(Capabilities);
        int curveOffsetMv = p.VoltageOffsetMv;
        int lockMv = 0;              // hoisted: the verify pass below needs it

        lock (_lock)
        {
            void Try(string what, Action a)
            {
                try { a(); Log?.Invoke($"{what}: ok"); }
                catch (Exception e) { errors.Add($"{what}: {e.Message}"); Log?.Invoke($"{what}: FAILED — {e.Message}"); }
            }

            // Order matters a little: raise power/temp limits before pushing clocks, lower clocks before lowering limits.
            bool raising = AppliedProfile == null || p.PowerLimitPercent >= AppliedProfile.PowerLimitPercent;
            if (raising)
            {
                if (Capabilities.CanSetPowerLimit) Try("Power limit", () => Backend.SetPowerLimit(GpuIndex, p.PowerLimitPercent));
                if (Capabilities.CanSetTempLimit) Try("Temp limit", () => Backend.SetTempLimit(GpuIndex, p.TempLimitC));
            }
            // One absolute voltage target drives both levers; they're mutually exclusive by design.
            // Older profiles (and the CLI's --volt/--uv) still set the two fields directly.
            int boostPct = p.VoltageBoostPercent;
            bool voltageIsOffset = Capabilities.VoltageStyle == VoltageControlStyle.Offset;
            if (voltageIsOffset)
            {
                // One signed offset, no ceiling arithmetic: the vendor applies it to the whole curve.
                curveOffsetMv = p.VoltageOffsetMv;
                boostPct = 0;
                Log?.Invoke($"Voltage offset: {curveOffsetMv:+#;-#;0} mV");
            }
            else
            {
                // Two independent levers, the way Afterburner exposes them:
                //   VoltageBoostPercent raises the ceiling above the top of the V/F table
                //   TargetVoltageMv caps the core at or below a chosen absolute voltage
                // They pull in opposite directions, but a card can legitimately carry both — a boost
                // for headroom with a cap holding it under load — so neither is derived from the other.
                //
                // A ceiling of 0 means the curve read failed. Falling through with it would make every
                // target look like "at or above stock" and write nothing, so treat the top of the
                // slider's range as the ceiling instead and let the cap through.
                int ceiling = StockCeilingMv > 0 ? StockCeilingMv : Capabilities.MaxVoltageMv;

                // Profiles written before the boost had its own control encoded it in the target
                // voltage, so a target above the ceiling meant "boost this far". Honour that when the
                // profile carries no boost of its own, or loading an old profile silently drops it.
                boostPct = p.VoltageBoostPercent;
                if (boostPct == 0 && p.TargetVoltageMv > ceiling)
                {
                    (boostPct, _) = VoltagePlan.Compute(p.TargetVoltageMv, ceiling, Capabilities.MaxVoltageMv);
                    if (boostPct > 0) Log?.Invoke($"Legacy profile: target {p.TargetVoltageMv} mV above ceiling → boost {boostPct}%");
                }

                // Below the ceiling the cap is enforced by the absolute lock, not by an offset that
                // would be measured from whichever "stock" figure we happened to pick.
                lockMv = p.TargetVoltageMv > 0 && p.TargetVoltageMv < ceiling ? p.TargetVoltageMv : 0;
                curveOffsetMv = 0;
                Log?.Invoke($"Voltage: boost {boostPct}%, cap {lockMv} mV vs stock ceiling {ceiling} mV " +
                            $"(curve top {Capabilities.StockMaxVoltageMv}, seen {ObservedMaxVoltageMv})");
            }

            if (Capabilities.CanSetVoltageBoost) Try("Voltage boost", () => Backend.SetVoltageBoost(GpuIndex, boostPct));

            // The undervolt is enforced by the voltage lock, which is independent of the p-state clock
            // offset — so unlike the old delta-table approach these no longer share storage and both
            // must be written. (Folding the core offset into the curve silently dropped it, because
            // the delta table can't reach the points a high cap needs.)
            // Either lever routes through SetVoltageCurveOffset: NVIDIA's curve flatten and AMD's
            // whole-curve offset. Gating this on CanSetVoltageCurve alone would drop the AMD write,
            // because AMD has no editable curve at all.
            if (Capabilities.CanSetVoltageCurve || Capabilities.VoltageStyle == VoltageControlStyle.Offset)
                Try("Voltage offset", () => Backend.SetVoltageCurveOffset(GpuIndex, curveOffsetMv, 0));
            if (Capabilities.CanSetCoreOffset)
                Try("Core offset", () => Backend.SetCoreOffset(GpuIndex, p.CoreOffsetMhz));
            if (Capabilities.CanSetMemoryOffset) Try("Memory offset", () => Backend.SetMemoryOffset(GpuIndex, p.MemoryOffsetMhz));

            // The cap goes in LAST, on purpose. The curve-offset and core-offset writes touch the same
            // delta table and have historically cleared it as a side effect; writing it after them
            // means nothing downstream can quietly undo it.
            if (Capabilities.VoltageStyle == VoltageControlStyle.Absolute)
                Try("Voltage cap", () => Backend.SetVoltageLock(GpuIndex, lockMv));

            if (!raising)
            {
                if (Capabilities.CanSetPowerLimit) Try("Power limit", () => Backend.SetPowerLimit(GpuIndex, p.PowerLimitPercent));
                if (Capabilities.CanSetTempLimit) Try("Temp limit", () => Backend.SetTempLimit(GpuIndex, p.TempLimitC));
            }

            if (Capabilities.CanSetZeroRpm)
                Try("Zero RPM", () => Backend.SetZeroRpm(GpuIndex, p.ZeroRpm));
            if (Capabilities.CanSetMemoryTiming)
                Try("Memory timing", () => Backend.SetMemoryTiming(GpuIndex, p.MemoryTimingLevel));

            if (Capabilities.CanSetFanSpeed)
            {
                switch (p.FanMode)
                {
                    case FanMode.Auto:
                        _activeCurve = null;
                        Try("Fan auto", () => Backend.SetFanAuto(GpuIndex));
                        break;
                    case FanMode.Fixed:
                        _activeCurve = null;
                        Try("Fan fixed", () => Backend.SetFanSpeed(GpuIndex, -1, p.FixedFanPercent));
                        break;
                    case FanMode.Curve:
                        if (Capabilities.FanCurveIsHardware)
                        {
                            // The driver runs it, so it survives this app closing — and no polling
                            // loop means no fan oscillation if the app is busy.
                            _activeCurve = null;
                            Try("Fan curve", () => Backend.SetFanCurve(GpuIndex, p.FanCurve));
                        }
                        else
                        {
                            _activeCurve = p.FanCurve.Clone();
                            _activeCurve.ResetState();
                            _lastCurveFanSent = double.NaN;
                            // First step happens on next poll using the live temperature.
                        }
                        break;
                }
            }

            if (ManualCurveActive && (Capabilities.CanSetVoltageCurve || Capabilities.CanSetCoreOffset))
            {
                errors.Add("Note: your hand-edited V/F curve was overwritten — the curve editor and these " +
                           "sliders share the same delta table. Re-open the curve editor to redo it.");
                ManualCurveActive = false;
            }

            AppliedProfile = p;

            // Verify-after-write, still holding the lock: the vendor libraries are not re-entrant and
            // the polling thread is reading telemetry through the same handle.
            try
            {
                var actual = Backend.ReadTuningState(GpuIndex);
                // Voltage boost is deliberately NOT verified: on several Ada cards the write takes effect
                // (measurable rise in core voltage) while GetCoreVoltageBoostPercent still reports 0, so a
                // read-back mismatch here would be a false alarm. Judge it by the live voltage instead.
                if (Capabilities.CanSetPowerLimit && Math.Abs(actual.PowerLimitPercent - p.PowerLimitPercent) > 1
                    && !errors.Any(e => e.StartsWith("Power limit")))
                    errors.Add($"Power limit: asked for {p.PowerLimitPercent}%, card reports {actual.PowerLimitPercent}%.");
                // An undervolt that only partially lands is worse than one that fails loudly: the card
                // keeps its stock ceiling and the UI would claim success. Read the flatten back.
                if (curveOffsetMv < 0 && Capabilities.VoltageStyle == VoltageControlStyle.Absolute)
                {
                    int want = curveOffsetMv, got = actual.VoltageOffsetMv;
                    if (got == 0)
                        errors.Add($"Undervolt: asked to flatten above {Capabilities.StockMaxVoltageMv + want} mV " +
                                   "but the curve is unchanged — the driver rejected the high-voltage points.");
                    else if (Math.Abs(got - want) > 15)
                        errors.Add($"Undervolt: asked for {want} mV, curve reads {got} mV — only part of the " +
                                   "curve moved. Re-apply once; the high-point write path is auto-detected on first use.");
                }
                if (Capabilities.VoltageStyle == VoltageControlStyle.Offset && curveOffsetMv != actual.VoltageOffsetMv
                    && !errors.Any(e => e.StartsWith("Voltage offset")))
                    errors.Add($"Voltage offset: asked for {curveOffsetMv:+#;-#;0} mV, card reports {actual.VoltageOffsetMv:+#;-#;0} mV.");

                // Read the lock back so an accepted-but-ignored write shows up here rather than as
                // "applied, nothing changed". The exact figure goes to the log either way.
                int gotLock = Backend.ReadVoltageLockMv(GpuIndex);
                if (gotLock >= 0 && !errors.Any(e => e.StartsWith("Voltage cap")))
                {
                    Log?.Invoke($"Voltage lock read-back: {gotLock} mV (asked {lockMv} mV) " +
                                $"via {Backend.VoltageLockMechanism}");
                    VoltageLockMechanism = Backend.VoltageLockMechanism;
                    // A zero read-back is ambiguous — some drivers accept the lock but never report it —
                    // so that case is logged, not shown as an error. A *different* voltage is unambiguous.
                    if (lockMv > 0 && Math.Abs(gotLock - lockMv) > 10 && gotLock > 0)
                        errors.Add($"Core voltage: asked for {lockMv} mV, card reports {gotLock} mV.");
                    else if (lockMv == 0 && gotLock > 0)
                        errors.Add($"Core voltage: a {gotLock} mV cap is still in place — press Apply again to clear it.");
                }
                if (Capabilities.CanSetCoreOffset && actual.CoreOffsetMhz != p.CoreOffsetMhz
                    && !errors.Any(e => e.StartsWith("Core offset")))
                    errors.Add($"Core offset: asked for {p.CoreOffsetMhz:+#;-#;0} MHz, card reports {actual.CoreOffsetMhz:+#;-#;0} MHz.");
            }
            catch (Exception e) { Log?.Invoke("Verify failed: " + e.Message); }
        }

        return errors;
    }

    public void ResetToDefaults()
    {
        lock (_lock)
        {
            _activeCurve = null;
            ManualCurveActive = false;
            Backend.ResetToDefaults(GpuIndex);
            AppliedProfile = TuningProfile.Stock(Capabilities, Device.Name);
            Log?.Invoke("Reset to driver defaults");
        }
    }

    // ------------------------------------------------------------------ V/F curve
    // The polling thread touches the backend every second under _lock; NVAPI is not re-entrant here,
    // so the curve editor must go through these rather than calling Backend directly from the UI thread.

    /// <summary>Read the editable V/F curve points. Empty when the card/driver doesn't expose them.</summary>
    public IReadOnlyList<VfCurveSample> ReadVfCurve()
    {
        lock (_lock) { return Backend.ReadVfCurve(GpuIndex); }
    }

    /// <summary>Write explicit per-point curve targets; every other point returns to stock. Empty clears the curve.</summary>
    public void SetVfCurveTargets(IReadOnlyList<VfCurveSample> targets)
    {
        lock (_lock)
        {
            Backend.SetVfCurveTargets(GpuIndex, targets);
            ManualCurveActive = targets.Count > 0;
        }
    }

    /// <summary>Read what the driver currently has, as a profile (used to seed the sliders on launch).</summary>
    public TuningProfile ReadCurrentAsProfile()
    {
        var s = Backend.ReadTuningState(GpuIndex);
        return new TuningProfile
        {
            Name = "Current",
            GpuName = Device.Name,
            CoreOffsetMhz = s.CoreOffsetMhz,
            MemoryOffsetMhz = s.MemoryOffsetMhz,
            PowerLimitPercent = s.PowerLimitPercent,
            TempLimitC = s.TempLimitC,
            VoltageBoostPercent = s.VoltageBoostPercent,
            VoltageOffsetMv = s.VoltageOffsetMv,
            ZeroRpm = s.ZeroRpm,
            MemoryTimingLevel = s.MemoryTimingLevel,
            // The cap is whatever lock is actually on the card, not something inferred from the boost:
            // those are separate levers now, and deriving one from the other made a pure boost read
            // back as a cap sitting above the ceiling. -1 means the backend can't report a lock, in
            // which case fall back to the old inference so the slider still lands somewhere sane.
            TargetVoltageMv = Backend.ReadVoltageLockMv(GpuIndex) is var lk && lk >= 0
                ? lk
                : VoltagePlan.ToTargetMv(s.VoltageBoostPercent, s.VoltageOffsetMv,
                                         Capabilities.StockMaxVoltageMv, Capabilities.MaxVoltageMv),
            FanMode = s.FanManual ? FanMode.Fixed : FanMode.Auto,
            FixedFanPercent = s.FanPercent
        };
    }

    // ------------------------------------------------------------------ polling

    /// <summary>
    /// Set while no window of ours is on screen (minimised to tray, monitor closed). The poll then
    /// does the least work that still keeps a fan curve running, and no driver call at all when no
    /// curve is active — a full sample is ~13 ms of synchronous NVAPI work, which is not something to
    /// be doing every second behind a full-screen game. Read on the poll thread, set from the UI.
    /// </summary>
    public volatile bool BackgroundMode;

    public void StartPolling(int intervalMs = 1000)
    {
        StopPolling();
        _pollCts = new CancellationTokenSource();
        var ct = _pollCts.Token;
        _pollTask = Task.Run(async () =>
        {
            while (!ct.IsCancellationRequested)
            {
                try
                {
                    if (BackgroundMode) PollBackground();
                    else PollForeground();
                }
                catch (Exception e)
                {
                    Log?.Invoke("Poll error: " + e.Message);
                }
                try { await Task.Delay(intervalMs, ct); } catch (TaskCanceledException) { }
            }
        }, ct);
    }

    private void PollForeground()
    {
        GpuTelemetry t;
        lock (_lock) { t = Backend.ReadTelemetry(GpuIndex); }
        Latest = t;
        NoteVoltageObservation(t.VoltageMv);
        History.Add(t);
        RunFanCurve(t);
        TelemetryUpdated?.Invoke(t);
    }

    /// <summary>
    /// Nothing is on screen, so a sample is only worth taking if a fan curve needs one — and then
    /// only the temperature. The reading is stepped into the curve and dropped: it is deliberately
    /// not stored in <see cref="Latest"/> or <see cref="History"/> and not published, because it has
    /// one valid field and would otherwise show up in the graphs as a run of zeroes. The graphs keep
    /// a gap for the time you were gaming, which is the truth.
    /// </summary>
    private void PollBackground()
    {
        bool curveActive;
        lock (_lock) { curveActive = _activeCurve != null; }
        if (!curveActive) return;

        GpuTelemetry t;
        lock (_lock) { t = Backend.ReadTemperatureOnly(GpuIndex); }
        RunFanCurve(t);
    }

    public void StopPolling()
    {
        _pollCts?.Cancel();
        // Wait it out rather than timing out: Dispose tears the vendor context down next, and a poll
        // still inside a native call would be handed a dead handle.
        try { _pollTask?.Wait(TimeSpan.FromSeconds(10)); } catch { }
        _pollCts = null; _pollTask = null;
    }

    private void RunFanCurve(GpuTelemetry t)
    {
        FanCurve? curve;
        lock (_lock) { curve = _activeCurve; }
        if (curve == null) return;

        var next = curve.Step(t.TemperatureC);
        if (next == null) return;
        int pct = (int)Math.Round(next.Value);
        if (!double.IsNaN(_lastCurveFanSent) && Math.Abs(pct - _lastCurveFanSent) < 1) return;
        try
        {
            lock (_lock) { Backend.SetFanSpeed(GpuIndex, -1, pct); }
            _lastCurveFanSent = pct;
        }
        catch (Exception e) { Log?.Invoke("Fan curve: " + e.Message); }
    }

    /// <summary>
    /// Safety: on exit (or crash handler) hand fans back to the driver so a fixed low fan speed
    /// can't persist after the app that was supposed to manage it is gone.
    /// </summary>
    public void ReleaseFanControl()
    {
        lock (_lock)
        {
            _activeCurve = null;
            if (Capabilities.CanSetFanSpeed)
            {
                try { Backend.SetFanAuto(GpuIndex); Log?.Invoke("Fans returned to auto"); } catch { }
            }
        }
    }

    public void Dispose()
    {
        StopPolling();
        Backend.Dispose();
    }
}
