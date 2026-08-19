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

    /// <summary>
    /// Highest core voltage seen while the boost was wound fully open; 0 until measured. Kept apart
    /// from <see cref="ObservedMaxVoltageMv"/> because the two answer different questions: where the
    /// card sits at stock, and how far the boost can push it.
    /// </summary>
    public int ObservedMaxBoostedVoltageMv { get; private set; }

    /// <summary>How far above the curve's top point a reading can be and still count as "stock".</summary>
    private const int StockVidToleranceMv = 25;

    /// <summary>
    /// Floor for an observed ceiling: the lowest voltage this card's own controls go down to.
    ///
    /// This used to be a fixed drop below the curve's top point, which was wrong — the table's top
    /// is the table's extent, not a bound on what the card reaches. A 5070 Ti's curve runs to
    /// 1240 mV against a ceiling of 1035, and measuring down from the top rejected the real
    /// measurement as implausible. The card's minimum tunable voltage is the honest floor; the
    /// load gate on <see cref="NoteVoltageObservation"/> is what actually keeps idle samples out.
    /// </summary>
    private int ObservedCeilingFloorMv =>
        Capabilities.MinVoltageMv > 0 ? Capabilities.MinVoltageMv : 700;

    /// <summary>
    /// Load below which a voltage sample says nothing about the ceiling — an idle card sits near
    /// 800 mV because it has nothing to do, not because that is as high as it goes.
    /// </summary>
    private const int CeilingSampleMinLoadPercent = 40;

    /// <summary>
    /// Stock ceiling: the curve's top point, corrected to the VID the card really uses.
    ///
    /// Only readings within one V/F step above the curve top are trusted. Anything higher was reached
    /// *because* a boost was applied, and folding that back in would raise the ceiling to the top of
    /// the slider — after which nothing is above stock, so Apply would compute neither a boost nor a
    /// cap and write nothing while still reporting success. Readings below the top are trusted much
    /// further, because a card that never reaches its table's last point is common and the table
    /// alone would then overstate the ceiling by tens of millivolts.
    /// </summary>
    public int StockCeilingMv
    {
        get
        {
            int curveTop = Capabilities.StockMaxVoltageMv;
            if (curveTop <= 0) return ObservedMaxVoltageMv > 0 ? ObservedMaxVoltageMv : Capabilities.MaxVoltageMv;
            // Nothing sampled under load yet, so the table's top is the only honest answer.
            if (ObservedMaxVoltageMv <= 0) return curveTop;
            return Math.Clamp(ObservedMaxVoltageMv, ObservedCeilingFloorMv, curveTop + StockVidToleranceMv);
        }
    }
    /// <summary>
    /// Feed a live voltage reading into the ceiling estimate. Only samples taken while the card is
    /// running its own voltage count: once we've boosted or capped it, what it reports says nothing
    /// about where stock sits. Idle samples are dropped too — see <paramref name="loadPercent"/>.
    /// </summary>
    public void NoteVoltageObservation(double mv, double loadPercent = 100)
    {
        if (double.IsNaN(mv) || mv >= 1400) return;
        // An idle reading is the card resting, not the card's limit. Letting it set the ceiling
        // would claim a card tops out hundreds of millivolts below where it really does.
        if (double.IsNaN(loadPercent) || loadPercent < CeilingSampleMinLoadPercent) return;

        // A full boost with nothing capping it measures the OTHER ceiling: how far this card can be
        // driven above stock. That is the only way to learn the boost headroom, which no NVAPI call
        // reports — see BoostCeilingMv.
        if (_liveBoostPercent >= FullBoostPercent && _liveLockMv <= 0)
        {
            if (mv > ObservedMaxBoostedVoltageMv) ObservedMaxBoostedVoltageMv = (int)Math.Round(mv);
            return;
        }

        if (!VoltageIsUntouched) return;
        int curveTop = Capabilities.StockMaxVoltageMv;
        // A reading more than one V/F step above the curve's top point is boost, not stock — keeping
        // it would let a single sample poison the ceiling for the rest of the session.
        if (curveTop > 0 && mv > curveTop + StockVidToleranceMv) return;
        if (mv > ObservedMaxVoltageMv) ObservedMaxVoltageMv = (int)Math.Round(mv);
    }

    /// <summary>
    /// Seed the ceiling estimate with a figure measured in an earlier session. Held to the same
    /// bounds as a live observation, so a stale or hand-edited settings file cannot push the ceiling
    /// somewhere the curve doesn't support.
    /// </summary>
    public void SeedObservedMaxVoltage(int mv)
    {
        if (mv <= 0 || mv >= 1400) return;
        int curveTop = Capabilities.StockMaxVoltageMv;
        if (curveTop > 0 && mv > curveTop + StockVidToleranceMv) return;
        if (mv < ObservedCeilingFloorMv) return;
        if (mv > ObservedMaxVoltageMv) ObservedMaxVoltageMv = mv;
    }

    /// <summary>As <see cref="SeedObservedMaxVoltage"/>, for the boosted ceiling.</summary>
    public void SeedObservedMaxBoostedVoltage(int mv)
    {
        if (mv <= 0 || mv >= 1400) return;
        // Must sit above stock (a boost that buys nothing is not a measurement of headroom) and no
        // higher than the compiled-in guess, which is generous rather than tight.
        if (mv <= StockCeilingMv || (Capabilities.MaxVoltageMv > 0 && mv > Capabilities.MaxVoltageMv)) return;
        if (mv > ObservedMaxBoostedVoltageMv) ObservedMaxBoostedVoltageMv = mv;
    }

    /// <summary>
    /// Rail ceilings to restore on Reset, in mV; 0 when unknown. Seeded from what was recorded the
    /// first time this GPU was seen — see <see cref="AppSettings.NvvddDefaultMaxByGpu"/> for why this
    /// cannot simply be read back from the driver.
    /// </summary>
    public int NvvddDefaultMaxMv { get; private set; }
    public int MsvddDefaultMaxMv { get; private set; }

    public void SeedRailDefaults(int nvvddMv, int msvddMv)
    {
        if (nvvddMv > 0) NvvddDefaultMaxMv = nvvddMv;
        if (msvddMv > 0) MsvddDefaultMaxMv = msvddMv;
    }

    /// <summary>Boost level an observation must be taken at to measure the full headroom.</summary>
    private const int FullBoostPercent = 100;

    /// <summary>Voltage state the card is actually carrying, refreshed when we change it.</summary>
    private int _liveBoostPercent, _liveLockMv, _liveRailMaxMv;

    /// <summary>
    /// Re-read the levers the card is holding. Needed because "has anything been applied" cannot be
    /// answered from <see cref="AppliedProfile"/> alone: a boost set in an earlier run, or by another
    /// tool, is still on the card when we start, and treating that as stock would record a boosted
    /// voltage as the stock ceiling.
    /// </summary>
    private void RefreshLiveVoltageState()
    {
        try
        {
            var live = Backend.ReadTuningState(GpuIndex);
            _liveBoostPercent = live.VoltageBoostPercent;
            _liveRailMaxMv = live.VoltageRailMaxMv;
            _liveLockMv = Math.Max(0, Backend.ReadVoltageLockMv(GpuIndex));
        }
        catch { _liveBoostPercent = 0; _liveLockMv = 0; _liveRailMaxMv = 0; }
    }

    /// <summary>True while no voltage lever is engaged, so live voltage still reflects stock.</summary>
    private bool VoltageIsUntouched
    {
        get
        {
            if (_liveBoostPercent != 0 || _liveLockMv > 0) return false;
            var a = AppliedProfile;
            if (a == null) return true;
            if (a.VoltageBoostPercent != 0 || a.VoltageOffsetMv != 0) return false;
            return a.TargetVoltageMv <= 0 || a.TargetVoltageMv >= Capabilities.StockMaxVoltageMv;
        }
    }

    /// <summary>
    /// The highest voltage this card reaches with the boost wound fully open.
    ///
    /// The compiled-in fallback adds a fixed headroom to the curve's top point, measured once on a
    /// 4070 Ti; NVAPI exposes no way to ask. That figure is per-card and can be far too generous — a
    /// 5070 Ti gains about 15 mV where the constant assumes 60 — which leaves the cap slider with
    /// travel the card cannot use. A measured value always wins over the guess.
    /// </summary>
    public int BoostCeilingMv
    {
        get
        {
            int stock = StockCeilingMv;
            int ceiling = ObservedMaxBoostedVoltageMv > stock
                ? ObservedMaxBoostedVoltageMv
                : Math.Max(stock, Capabilities.MaxVoltageMv);
            // Raising the rail's own ceiling lifts the roof everything else works under, so a cap
            // measured before that happened would leave the slider unable to reach the voltages the
            // card can now select.
            return Math.Max(ceiling, _liveRailMaxMv);
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
            // Whatever the card is already carrying — possibly from an earlier run — decides whether
            // its live voltage can be trusted as "stock".
            RefreshLiveVoltageState();
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
        AddClampNotes(errors, profile, p);
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
                    (boostPct, _) = VoltagePlan.Compute(p.TargetVoltageMv, ceiling, BoostCeilingMv);
                    if (boostPct > 0) Log?.Invoke($"Legacy profile: target {p.TargetVoltageMv} mV above ceiling → boost {boostPct}%");
                }

                // Below the ceiling the cap is enforced by the absolute lock, not by an offset that
                // would be measured from whichever "stock" figure we happened to pick.
                lockMv = p.TargetVoltageMv > 0 && p.TargetVoltageMv < ceiling ? p.TargetVoltageMv : 0;

                // A negative offset is how the CLI asks for an undervolt: --uv -90 means "hold 90 mV
                // under the ceiling". It used to be zeroed here along with the curve delta, so the
                // command reported success and changed nothing at all. This backend caps by absolute
                // voltage, so convert. An explicit cap wins, being the more specific instruction.
                if (lockMv == 0 && p.VoltageOffsetMv < 0 && ceiling > 0)
                {
                    lockMv = Math.Max(Capabilities.MinVoltageMv, ceiling + p.VoltageOffsetMv);
                    p.TargetVoltageMv = lockMv;   // keep the applied profile describing what was done
                    Log?.Invoke($"Undervolt {p.VoltageOffsetMv:+#;-#;0} mV → cap {lockMv} mV (ceiling {ceiling} mV)");
                }

                curveOffsetMv = 0;
                Log?.Invoke($"Voltage: boost {boostPct}%, cap {lockMv} mV vs stock ceiling {ceiling} mV " +
                            $"(curve top {Capabilities.StockMaxVoltageMv}, seen {ObservedMaxVoltageMv})");
            }

            if (Capabilities.CanSetVoltageBoost) Try("Voltage boost", () => Backend.SetVoltageBoost(GpuIndex, boostPct));
            // Armed or not, the hardware ends up where the gate says it should be: disarmed means
            // "back to the driver's own values", not "whatever the last run happened to leave".
            WriteXoc(p, Try);

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

            // The levers just moved, so what counts as a stock voltage reading moved with them.
            RefreshLiveVoltageState();
        }

        return errors;
    }

    /// <summary>
    /// Prefix marking a message as informational rather than a failure. <see cref="Apply"/> returns
    /// both through one list, and the front-ends split on this: a run whose only messages are notes
    /// applied cleanly and must not be reported as an error.
    /// </summary>
    public const string NotePrefix = "Note:";

    /// <summary>True when every message is a note, i.e. the apply itself succeeded.</summary>
    public static bool OnlyNotes(IReadOnlyList<string> messages) =>
        messages.Count > 0 && messages.All(m => m.StartsWith(NotePrefix, StringComparison.Ordinal));

    /// <summary>
    /// Note every value the card's limits had to narrow. Clamping itself is right — the driver's
    /// range is the range — but applying a different number than the one asked for without saying
    /// so is how "+3500 MHz memory" read as success while the card actually ran +3000.
    /// </summary>
    private void AddClampNotes(List<string> notes, TuningProfile asked, TuningProfile applied)
    {
        void Note(string what, int a, int b, string unit)
        {
            if (a != b)
                notes.Add($"{NotePrefix} {what} {a}{unit} is outside this card's range — applied {b}{unit} instead.");
        }

        Note("core offset", asked.CoreOffsetMhz, applied.CoreOffsetMhz, " MHz");
        Note("memory offset", asked.MemoryOffsetMhz, applied.MemoryOffsetMhz, " MHz");
        Note("power limit", asked.PowerLimitPercent, applied.PowerLimitPercent, "%");
        // Only when the card has a thermal policy at all. Where it hasn't, the profile still carries
        // a default nobody asked for — a hidden, driver-owned limit on RDNA 4 — and noting that on
        // every apply is noise. The front-end that knows the user actually asked says so instead.
        if (Capabilities.CanSetTempLimit)
            Note("temperature limit", asked.TempLimitC, applied.TempLimitC, "°C");
        Note("voltage boost", asked.VoltageBoostPercent, applied.VoltageBoostPercent, "%");
        Note("undervolt", asked.VoltageOffsetMv, applied.VoltageOffsetMv, " mV");
        // Only worth reporting when the gate is open; disarmed, none of these were written.
        if (asked.XocEnabled)
        {
            if (asked.VoltageRailMaxMv > 0)
                Note("core rail ceiling", asked.VoltageRailMaxMv, applied.VoltageRailMaxMv, " mV");
            if (asked.MsvddRailMaxMv > 0)
                Note("MSVDD ceiling", asked.MsvddRailMaxMv, applied.MsvddRailMaxMv, " mV");
            if (asked.VoltageRailFloorMv > 0)
                Note("core rail floor", asked.VoltageRailFloorMv, applied.VoltageRailFloorMv, " mV");
            if (asked.MsvddRailFloorMv > 0)
                Note("MSVDD floor", asked.MsvddRailFloorMv, applied.MsvddRailFloorMv, " mV");
            if (Capabilities.CanSetXbarOffset)
                Note("crossbar offset", asked.XbarOffsetMhz, applied.XbarOffsetMhz, " MHz");
        }
        // Only meaningful when a cap was actually asked for; 0 means "no cap" and never clamps.
        if (asked.TargetVoltageMv > 0)
            Note("voltage cap", asked.TargetVoltageMv, applied.TargetVoltageMv, " mV");
        // An auto-fan profile still carries whatever fixed duty it last had, so only speak up when
        // that duty is the thing actually being applied.
        if (asked.FanMode == FanMode.Fixed)
            Note("fan speed", asked.FixedFanPercent, applied.FixedFanPercent, "%");
    }

    // ------------------------------------------------------------------ extreme OC (XOC)

    /// <summary>
    /// Write the two rails and the crossbar, or put them back where the driver had them. Runs on
    /// every apply as well as from the XOC window's Enable/Disable, so the card always matches the
    /// gate instead of drifting from what an earlier session wrote.
    /// </summary>
    private void WriteXoc(TuningProfile p, Action<string, Action> Try)
    {
        if (p.XocEnabled)
        {
            // Rail ceiling is independent of both boost and cap: it moves the roof, they work under it.
            if (Capabilities.CanSetVoltageRail && p.VoltageRailMaxMv > 0)
                Try("Core rail ceiling", () => Backend.SetVoltageRailMax(GpuIndex, p.VoltageRailMaxMv));
            if (Capabilities.CanSetMsvddRail && p.MsvddRailMaxMv > 0)
                Try("MSVDD ceiling", () => Backend.SetMsvddRailMax(GpuIndex, p.MsvddRailMaxMv));
            if (Capabilities.CanSetVoltageRail && p.VoltageRailFloorMv > 0)
                Try("Core rail floor", () => Backend.SetVoltageRailFloor(GpuIndex, p.VoltageRailFloorMv));
            if (Capabilities.CanSetMsvddRail && p.MsvddRailFloorMv > 0)
                Try("MSVDD floor", () => Backend.SetMsvddRailFloor(GpuIndex, p.MsvddRailFloorMv));
            if (Capabilities.CanSetXbarOffset)
                Try("Crossbar offset", () => Backend.SetXbarOffset(GpuIndex, p.XbarOffsetMhz));
            return;
        }

        // Disarmed. Ceilings go back to what was recorded before anything touched them; a default we
        // never saw is left alone rather than guessed at, because guessing low browns the card out.
        // Floors have a real default of zero offset, unlike the ceilings.
        if (Capabilities.CanSetVoltageRail && NvvddDefaultMaxMv > 0)
            Try("Core rail ceiling", () => Backend.SetVoltageRailMax(GpuIndex, NvvddDefaultMaxMv));
        if (Capabilities.CanSetMsvddRail && MsvddDefaultMaxMv > 0)
            Try("MSVDD ceiling", () => Backend.SetMsvddRailMax(GpuIndex, MsvddDefaultMaxMv));
        if (Capabilities.CanSetVoltageRail)
            Try("Core rail floor", () => Backend.SetVoltageRailFloor(GpuIndex, 0));
        if (Capabilities.CanSetMsvddRail)
            Try("MSVDD floor", () => Backend.SetMsvddRailFloor(GpuIndex, 0));
        if (Capabilities.CanSetXbarOffset)
            Try("Crossbar offset", () => Backend.SetXbarOffset(GpuIndex, 0));
    }

    /// <summary>
    /// Arm or disarm the XOC levers and write them now, leaving clocks, power, temp and fan alone.
    /// The profile supplies the values to arm with; disarming ignores them.
    /// </summary>
    public IReadOnlyList<string> SetXocEnabled(TuningProfile profile, bool enabled)
    {
        var errors = new List<string>();
        var p = profile.Clone();
        p.XocEnabled = enabled;
        p.ClampTo(Capabilities);

        lock (_lock)
        {
            void Try(string what, Action a)
            {
                try { a(); Log?.Invoke($"{what}: ok"); }
                catch (Exception e) { errors.Add($"{what}: {e.Message}"); Log?.Invoke($"{what}: FAILED - {e.Message}"); }
            }

            WriteXoc(p, Try);

            // Keep the applied profile describing the card, so the summary line and a later Revert
            // both agree with what is actually on it.
            if (AppliedProfile != null)
            {
                AppliedProfile.XocEnabled = enabled;
                AppliedProfile.VoltageRailMaxMv = enabled ? p.VoltageRailMaxMv : 0;
                AppliedProfile.MsvddRailMaxMv = enabled ? p.MsvddRailMaxMv : 0;
                AppliedProfile.VoltageRailFloorMv = enabled ? p.VoltageRailFloorMv : 0;
                AppliedProfile.MsvddRailFloorMv = enabled ? p.MsvddRailFloorMv : 0;
                AppliedProfile.XbarOffsetMhz = enabled ? p.XbarOffsetMhz : 0;
            }
            // The NVVDD ceiling is one of the inputs to the boosted ceiling, so the figure the rest
            // of the UI works from is now stale.
            RefreshLiveVoltageState();
            Log?.Invoke($"XOC {(enabled ? "enabled" : "disabled")}");
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
            // Rails last: the backend deliberately leaves them alone because only this layer knows
            // what they were before anything touched them.
            if (Capabilities.CanSetVoltageRail && NvvddDefaultMaxMv > 0)
                try { Backend.SetVoltageRailMax(GpuIndex, NvvddDefaultMaxMv); } catch (GpuBackendException) { }
            if (Capabilities.CanSetMsvddRail && MsvddDefaultMaxMv > 0)
                try { Backend.SetMsvddRailMax(GpuIndex, MsvddDefaultMaxMv); } catch (GpuBackendException) { }
            // Floors have a real default of zero offset, unlike the ceilings: MSVDD ships with a
            // maximum offset already applied but neither rail ships with a minimum offset.
            if (Capabilities.CanSetVoltageRail)
                try { Backend.SetVoltageRailFloor(GpuIndex, 0); } catch (GpuBackendException) { }
            if (Capabilities.CanSetMsvddRail)
                try { Backend.SetMsvddRailFloor(GpuIndex, 0); } catch (GpuBackendException) { }
            AppliedProfile = TuningProfile.Stock(Capabilities, Device.Name);
            RefreshLiveVoltageState();
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
            VoltageRailMaxMv = s.VoltageRailMaxMv,
            MsvddRailMaxMv = s.MsvddRailMaxMv,
            VoltageRailFloorMv = s.VoltageRailFloorMv,
            MsvddRailFloorMv = s.MsvddRailFloorMv,
            XbarOffsetMhz = s.XbarOffsetMhz,
            ZeroRpm = s.ZeroRpm,
            MemoryTimingLevel = s.MemoryTimingLevel,
            // The cap is whatever lock is actually on the card, not something inferred from the boost:
            // those are separate levers now, and deriving one from the other made a pure boost read
            // back as a cap sitting above the ceiling. -1 means the backend can't report a lock, in
            // which case fall back to the old inference so the slider still lands somewhere sane.
            TargetVoltageMv = Backend.ReadVoltageLockMv(GpuIndex) is var lk && lk >= 0
                ? lk
                : VoltagePlan.ToTargetMv(s.VoltageBoostPercent, s.VoltageOffsetMv,
                                         StockCeilingMv, BoostCeilingMv),
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
        NoteVoltageObservation(t.VoltageMv, t.GpuLoadPercent);
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
    ///
    /// That reasoning only holds for a curve *this* app runs. Where the curve lives in the driver
    /// the hardware keeps running it whether this process exists or not, so there is nothing unsafe
    /// to hand back — and resetting it here silently threw away the user's fan settings on every
    /// exit, including ones a startup profile had just applied seconds earlier.
    /// </summary>
    public void ReleaseFanControl()
    {
        lock (_lock)
        {
            _activeCurve = null;
            if (Capabilities.CanSetFanSpeed && !Capabilities.FanCurveIsHardware)
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
