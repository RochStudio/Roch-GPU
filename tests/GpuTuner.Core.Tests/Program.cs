using GpuTuner.Core.Backends.Amd;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Backends.Nvidia;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

int pass = 0, fail = 0;
void Check(string name, bool cond) { if (cond) pass++; else { fail++; Console.WriteLine("FAIL: " + name); } }
void Eq(string name, double expected, double actual, double tol = 1e-6) => Check($"{name} (expected {expected}, got {actual})", Math.Abs(expected - actual) <= tol);

// ---- FanCurve interpolation
var curve = new FanCurve { Points = new() { new(30, 20), new(60, 50), new(80, 100) } };
Eq("below first point", 20, curve.Evaluate(10));
Eq("at first point", 20, curve.Evaluate(30));
Eq("midpoint 45C", 35, curve.Evaluate(45));
Eq("70C", 75, curve.Evaluate(70));
Eq("above last", 100, curve.Evaluate(95));
curve.ZeroRpmBelowFirstPoint = true;
Eq("zero rpm below first", 0, curve.Evaluate(25));
Eq("zero rpm off at first point", 20, curve.Evaluate(30));

// unsorted points still evaluate correctly
var unsorted = new FanCurve { Points = new() { new(80, 100), new(30, 20), new(60, 50) } };
Eq("unsorted 45C", 35, unsorted.Evaluate(45));

// ---- Hysteresis
var h = new FanCurve { Points = new() { new(40, 30), new(80, 100) }, HysteresisC = 5, MinimumStepPercent = 2 };
var s1 = h.Step(60);            // first call always returns
Check("first step returns", s1.HasValue && Math.Abs(s1.Value - 65) < 1e-6);
Check("tiny change ignored", h.Step(60.5) == null);
var s2 = h.Step(70);            // rising → immediate
Check("rising follows immediately", s2.HasValue && Math.Abs(s2.Value - 82.5) < 1e-6);
Check("falling within hysteresis ignored", h.Step(66) == null);
var s3 = h.Step(64);            // 70-5=65 → 64 clears the band
Check("falling past hysteresis applies", s3.HasValue && Math.Abs(s3.Value - 72) < 1e-6);

// ---- Normalize clamps + sorts
var n = new FanCurve { Points = new() { new(200, 150), new(-5, -10), new(50, 50) } };
n.Normalize();
Check("normalize sorted", n.Points[0].TemperatureC == 0 && n.Points[2].TemperatureC == 110);
Check("normalize clamped", n.Points[0].FanPercent == 0 && n.Points[2].FanPercent == 100);

// ---- Profile clamp
var caps = new GpuCapabilities { CoreOffsetMinMhz = -200, CoreOffsetMaxMhz = 200, MemoryOffsetMinMhz = -500, MemoryOffsetMaxMhz = 1000, PowerLimitMinPercent = 60, PowerLimitMaxPercent = 110, TempLimitMinC = 65, TempLimitMaxC = 88 };
var p = new TuningProfile { CoreOffsetMhz = 999, MemoryOffsetMhz = -9999, PowerLimitPercent = 200, TempLimitC = 10 };
p.ClampTo(caps);
Check("clamp core", p.CoreOffsetMhz == 200);
Check("clamp mem", p.MemoryOffsetMhz == -500);
Check("clamp power", p.PowerLimitPercent == 110);
Check("clamp temp", p.TempLimitC == 65);

// ---- ProfileStore round trip
var dir = Path.Combine(Path.GetTempPath(), "gputuner-tests-" + Guid.NewGuid().ToString("N"));
var store = new ProfileStore(dir);
var prof = new TuningProfile { Name = "My UV", CoreOffsetMhz = 150, MemoryOffsetMhz = 800, PowerLimitPercent = 85, TempLimitC = 78, FanMode = FanMode.Curve };
prof.FanCurve.Points = new() { new(35, 25), new(75, 90) };
prof.FanCurve.HysteresisC = 4;
store.Save(prof);
var back = store.Load("My UV");
Check("store load not null", back != null);
Check("store core", back!.CoreOffsetMhz == 150);
Check("store fanmode enum", back.FanMode == FanMode.Curve);
Check("store curve points", back.FanCurve.Points.Count == 2 && back.FanCurve.Points[1].FanPercent == 90);
Check("store hysteresis", Math.Abs(back.FanCurve.HysteresisC - 4) < 1e-9);
Check("store list", store.ListProfileNames().SequenceEqual(new[] { "My UV" }));
Check("store delete", store.Delete("My UV") && store.ListProfileNames().Count == 0);
var settings = new AppSettings { StartupProfile = "X", ApplyOnStartup = true, PollIntervalMs = 500 };
store.SaveSettings(settings);
var sBack = store.LoadSettings();
Check("settings roundtrip", sBack.StartupProfile == "X" && sBack.ApplyOnStartup && sBack.PollIntervalMs == 500);
Directory.Delete(dir, true);

// ---- TelemetryHistory ring buffer
var hist = new TelemetryHistory(3);
for (int i = 1; i <= 5; i++) hist.Add(new GpuTelemetry { CoreClockMhz = i });
var snap = hist.Series(t => t.CoreClockMhz);
Check("ring keeps last 3 in order", snap.SequenceEqual(new double[] { 3, 4, 5 }));

// ---- TuningService against mock: apply, fan curve drives fan, release returns auto
using (var svc = new TuningService(new MockBackend()))
{
    svc.Initialize();
    var errs = svc.Apply(new TuningProfile { CoreOffsetMhz = 120, MemoryOffsetMhz = 500, PowerLimitPercent = 90, TempLimitC = 80, VoltageBoostPercent = 25, FanMode = FanMode.Fixed, FixedFanPercent = 66 });
    Check("apply no errors", errs.Count == 0);
    var st = svc.Backend.ReadTuningState(0);
    Check("mock applied core", st.CoreOffsetMhz == 120);
    Check("mock applied voltage boost", st.VoltageBoostPercent == 25);
    Check("mock applied fan fixed", st.FanManual && st.FanPercent == 66);

    // out-of-range value gets clamped rather than rejected
    svc.Apply(new TuningProfile { CoreOffsetMhz = 5000 });
    Check("apply clamps", svc.Backend.ReadTuningState(0).CoreOffsetMhz == 1000);

    var cp = new TuningProfile { FanMode = FanMode.Curve };
    cp.FanCurve.Points = new() { new(0, 77), new(120, 77) }; // flat curve → always 77 %
    svc.Apply(cp);
    svc.StartPolling(20);
    Thread.Sleep(300);
    svc.StopPolling();
    var st2 = svc.Backend.ReadTuningState(0);
    Check("curve drove fan to 77", st2.FanManual && st2.FanPercent == 77);

    svc.ReleaseFanControl();
    Check("release → auto", !svc.Backend.ReadTuningState(0).FanManual);

    svc.ResetToDefaults();
    Check("reset", svc.Backend.ReadTuningState(0).CoreOffsetMhz == 0);
}

// ---- Clock range: a window the card is held inside, not an offset, and not behind the XOC gate.
using (var svc = new TuningService(new MockBackend()))
{
    svc.Initialize();
    svc.Apply(new TuningProfile { ClockLockMinMhz = 1500, ClockLockMaxMhz = 1800 });
    var st = svc.Backend.ReadTuningState(0);
    Check("clock range applied", st.LockedClockMinMhz == 1500 && st.LockedClockMaxMhz == 1800);

    // 0/0 is "unpinned", and an apply that does not ask for a window has to clear one left behind.
    svc.Apply(new TuningProfile());
    st = svc.Backend.ReadTuningState(0);
    Check("clock range cleared when unset", st.LockedClockMinMhz == 0 && st.LockedClockMaxMhz == 0);

    // Not gated: a clock lock holds the card inside a range, it cannot raise a voltage.
    svc.Apply(new TuningProfile { XocEnabled = false, ClockLockMinMhz = 900, ClockLockMaxMhz = 1200 });
    st = svc.Backend.ReadTuningState(0);
    Check("clock range ignores the XOC gate", st.LockedClockMinMhz == 900);

    svc.ResetToDefaults();
    st = svc.Backend.ReadTuningState(0);
    Check("reset unpins the clock", st.LockedClockMinMhz == 0 && st.LockedClockMaxMhz == 0);
}

{
    var lockCaps = new GpuCapabilities { CanLockClocks = true, ClockLockMinMhz = 210, ClockLockMaxMhz = 3090 };
    var p1 = new TuningProfile { ClockLockMinMhz = 50, ClockLockMaxMhz = 9000 };
    p1.ClampTo(lockCaps);
    Check("clock range clamps to what the driver locks", p1.ClockLockMinMhz == 210 && p1.ClockLockMaxMhz == 3090);

    // A floor above its ceiling is not a window the driver can honour.
    var p2 = new TuningProfile { ClockLockMinMhz = 2000, ClockLockMaxMhz = 1000 };
    p2.ClampTo(lockCaps);
    Check("inverted clock window collapses rather than inverting", p2.ClockLockMinMhz <= p2.ClockLockMaxMhz);

    // Untouched profiles stay untouched: 0/0 must not become 210/210 and silently pin the card.
    var p3 = new TuningProfile();
    p3.ClampTo(lockCaps);
    Check("an unset clock range stays unset", p3.ClockLockMinMhz == 0 && p3.ClockLockMaxMhz == 0);
}

// ---- XOC gate: the rails and crossbar are written only while armed, and disarming puts them back.
// Regression guard for the reason the gate exists — a rail ceiling left standing from an earlier
// session is exactly the state that browns a card out on the next boot.
using (var svc = new TuningService(new MockBackend()))
{
    svc.Initialize();
    svc.SeedRailDefaults(1035, 985);

    var armed = new TuningProfile
    {
        XocEnabled = true,
        VoltageRailMaxMv = 1075, MsvddRailMaxMv = 1050,
        VoltageRailFloorMv = 850, MsvddRailFloorMv = 850, XbarOffsetMhz = 100,
        SysOffsetMhz = 45, VideoOffsetMhz = 30
    };
    svc.Apply(armed);
    var st = svc.Backend.ReadTuningState(0);
    Check("xoc armed writes nvvdd", st.VoltageRailMaxMv == 1075);
    Check("xoc armed writes msvdd", st.MsvddRailMaxMv == 1050);
    Check("xoc armed writes floors", st.VoltageRailFloorMv == 850 && st.MsvddRailFloorMv == 850);
    Check("xoc armed writes xbar", st.XbarOffsetMhz == 100);
    // SYS and video ride the same gate as the crossbar: same private family, same kind of lever.
    Check("xoc armed writes sys and video", st.SysOffsetMhz == 45 && st.VideoOffsetMhz == 30);

    // Same values, gate shut: every one of them goes back to the driver's own figure, and the two
    // ceilings go to their own separate defaults rather than a shared one.
    svc.Apply(new TuningProfile
    {
        XocEnabled = false,
        VoltageRailMaxMv = 1075, MsvddRailMaxMv = 1050,
        VoltageRailFloorMv = 850, MsvddRailFloorMv = 850, XbarOffsetMhz = 100,
        SysOffsetMhz = 45, VideoOffsetMhz = 30
    });
    st = svc.Backend.ReadTuningState(0);
    Check("xoc disarmed restores nvvdd default", st.VoltageRailMaxMv == 1035);
    Check("xoc disarmed restores msvdd default", st.MsvddRailMaxMv == 985);
    Check("xoc disarmed zeroes floors", st.VoltageRailFloorMv == 0 && st.MsvddRailFloorMv == 0);
    Check("xoc disarmed zeroes xbar", st.XbarOffsetMhz == 0);
    Check("xoc disarmed zeroes sys and video", st.SysOffsetMhz == 0 && st.VideoOffsetMhz == 0);

    // SetXocEnabled is the window's Enable/Disable: the gated levers move, nothing else does.
    svc.Apply(new TuningProfile { CoreOffsetMhz = 150, PowerLimitPercent = 110 });
    svc.SetXocEnabled(armed, true);
    st = svc.Backend.ReadTuningState(0);
    Check("SetXocEnabled(true) writes rails", st.VoltageRailMaxMv == 1075 && st.XbarOffsetMhz == 100);
    Check("SetXocEnabled leaves core alone", st.CoreOffsetMhz == 150);
    Check("SetXocEnabled leaves power alone", st.PowerLimitPercent == 110);

    svc.SetXocEnabled(armed, false);
    st = svc.Backend.ReadTuningState(0);
    Check("SetXocEnabled(false) restores rails", st.VoltageRailMaxMv == 1035 && st.XbarOffsetMhz == 0);
    Check("SetXocEnabled(false) leaves core alone", st.CoreOffsetMhz == 150);

    // An unknown default is left alone rather than guessed at: guessing low browns the card out.
    using (var bare = new TuningService(new MockBackend()))
    {
        bare.Initialize();
        bare.Apply(new TuningProfile { XocEnabled = true, VoltageRailMaxMv = 1100 });
        bare.Apply(new TuningProfile { XocEnabled = false });
        Check("xoc disarm without a recorded default leaves the ceiling put",
              bare.Backend.ReadTuningState(0).VoltageRailMaxMv == 1100);
    }

    // A profile round-trips the gate, so a saved tune cannot come back with the rails silently armed.
    Check("gate survives clone", armed.Clone().XocEnabled);
    Check("stock profile is disarmed", !TuningProfile.Stock(svc.Capabilities, "x").XocEnabled);
}

// ---- Curve span: the V/F curve runs through BOTH struct regions, not just the "GPU" one.
// Regression: the struct-based reader stopped at the 80-entry GPU array and reported a 4070 Ti's
// curve as 80 points ending at 945 mV, while the raw reader saw all 103 ending at 1090. Anything
// measured against the short curve — the stock ceiling, an undervolt target — came out wrong.
{
    // TotalPoints is the buffer's capacity, mask-bounded - not the struct's field split. It used to
    // return 80+23=103 on the reasoning that a 4070 Ti's 103 points were "exactly the mask's bit
    // count"; they were not. The mask is 128 bits, and a 5070 Ti returns 127 points in the same
    // buffer, so the split reached 81% of the curve and left the top slots unwritten on a reset.
    Check("curve span is the mask, not the struct split", NvApiPrivate.TotalPoints(80) == 128);
    Check("span never exceeds the mask", NvApiPrivate.TotalPoints(128) == NvApiPrivate.MaskPoints);
    Check("span covers a 127-point Blackwell curve", NvApiPrivate.TotalPoints(80) >= 127);
    Check("span covers a 103-point Ada curve", NvApiPrivate.TotalPoints(80) >= 103);
    Check("trailing array defaults to the second", NvApiPrivate.TrailingArray == 1);
}

// ---- Every real backend must implement the cheap background read itself.
// IGpuBackend.ReadTemperatureOnly has a default that falls back to a full telemetry sample, which is
// correct but is the whole cost the background poll exists to avoid. A backend that quietly inherits
// it pays that cost on every tick of a fan curve with nothing on screen, and nothing else notices.
{
    foreach (var t in new[] { typeof(NvApiBackend), typeof(GpuTuner.Core.Backends.Amd.AdlBackend) })
        Check($"{t.Name} declares its own ReadTemperatureOnly",
              t.GetMethod("ReadTemperatureOnly", new[] { typeof(int) })?.DeclaringType == t);
}

// ---- Practical clock ranges narrow the driver's, and never widen it
{
    // Ada reports the width of its delta field, not a tunable range.
    var c = ClockStep.Narrow(-1000, 1000, ClockStep.CoreOffsetPracticalMinMhz, ClockStep.CoreOffsetPracticalMaxMhz);
    Check("core narrowed to -150..+495", c == (-150, 495));
    // Both ends must sit on the 15 MHz grid, or the slider has a stop the card can never occupy.
    Check("core practical ends are on the grid",
          ClockStep.CoreOffsetPracticalMinMhz % ClockStep.CoreMhz == 0 &&
          ClockStep.CoreOffsetPracticalMaxMhz % ClockStep.CoreMhz == 0);
    Check("crossbar practical ends are on the grid",
          ClockStep.XbarOffsetPracticalMinMhz % ClockStep.CoreMhz == 0 &&
          ClockStep.XbarOffsetPracticalMaxMhz % ClockStep.CoreMhz == 0);
    // The 5% over-voltage grid has to reach both ends of a 0..100 slider.
    Check("voltage boost grid divides 100", 100 % ClockStep.VoltageBoostPercent == 0);
    Check("voltage boost snaps to fives",
          ClockStep.SnapWithin(63, ClockStep.VoltageBoostPercent, 0, 100) == 65 &&
          ClockStep.SnapWithin(100, ClockStep.VoltageBoostPercent, 0, 100) == 100 &&
          ClockStep.SnapWithin(0, ClockStep.VoltageBoostPercent, 0, 100) == 0);
    var m = ClockStep.Narrow(-1000, 4000, ClockStep.MemoryOffsetPracticalMinMhz, ClockStep.MemoryOffsetPracticalMaxMhz);
    Check("memory narrowed to -250..+4000", m == (-250, 4000));

    // A card that already reports less keeps its own range - this must never widen anything.
    Check("narrower driver range wins", ClockStep.Narrow(-100, 200, -300, 750) == (-100, 200));
    Check("one-sided narrowing", ClockStep.Narrow(-1000, 200, -300, 750) == (-300, 200));

    // The ends must land on the snapping grid, or the slider cannot reach its own limit.
    Eq("core min is on the 15 grid", ClockStep.CoreOffsetPracticalMinMhz,
       ClockStep.SnapWithin(ClockStep.CoreOffsetPracticalMinMhz, ClockStep.CoreMhz, -300, 750));
    Eq("core max is on the 15 grid", ClockStep.CoreOffsetPracticalMaxMhz,
       ClockStep.SnapWithin(ClockStep.CoreOffsetPracticalMaxMhz, ClockStep.CoreMhz, -300, 750));
    Eq("memory min is on the 25 grid", ClockStep.MemoryOffsetPracticalMinMhz,
       ClockStep.SnapWithin(ClockStep.MemoryOffsetPracticalMinMhz, ClockStep.MemoryMhz, -250, 4000));
    Eq("memory max is on the 25 grid", ClockStep.MemoryOffsetPracticalMaxMhz,
       ClockStep.SnapWithin(ClockStep.MemoryOffsetPracticalMaxMhz, ClockStep.MemoryMhz, -250, 4000));

    // A driver range with no overlap at all is left alone rather than inverted.
    Check("disjoint range left alone", ClockStep.Narrow(2000, 3000, -300, 750) == (2000, 3000));
}

// ---- ClockStep: the core offset snaps to the driver's 15 MHz grid
{
    const int s = ClockStep.CoreMhz;
    Check("core step is 15", s == 15);
    Eq("exact multiple is untouched", 150, ClockStep.Snap(150, s));
    Eq("rounds down below halfway", 15, ClockStep.Snap(20, s));
    Eq("rounds up past halfway", 30, ClockStep.Snap(23, s));
    // Anchored on zero, not on the slider minimum: stock has to stay reachable.
    Eq("zero stays zero", 0, ClockStep.Snap(0, s));
    Eq("small values collapse to stock", 0, ClockStep.Snap(7, s));
    // Negatives snap the same way as positives rather than drifting one step down.
    Eq("negative exact", -150, ClockStep.Snap(-150, s));
    Eq("negative rounds toward zero below halfway", 0, ClockStep.Snap(-7, s));
    Eq("negative rounds away past halfway", -15, ClockStep.Snap(-8, s));
    Eq("negative symmetric with positive", -30, ClockStep.Snap(-23, s));
    // A step of 1 (or nonsense) must not mangle the value.
    Eq("step 1 is a no-op", 137, ClockStep.Snap(137, 1));

    // SnapWithin: the driver's own limits are not multiples of 15, so a plain clamp would hand back
    // an off-grid endpoint (998 -> 1005 -> clamp 1000). Step back to the nearest in-range multiple.
    Eq("top of range stays on the grid", 990, ClockStep.SnapWithin(998, s, -1000, 1000));
    Eq("above the top clamps on-grid", 990, ClockStep.SnapWithin(5000, s, -1000, 1000));
    Eq("bottom of range stays on the grid", -990, ClockStep.SnapWithin(-998, s, -1000, 1000));
    Eq("below the bottom clamps on-grid", -990, ClockStep.SnapWithin(-5000, s, -1000, 1000));
    Eq("mid-range behaves like Snap", 135, ClockStep.SnapWithin(137, s, -1000, 1000));
    Eq("zero survives", 0, ClockStep.SnapWithin(0, s, -1000, 1000));
    // A limit that is itself on the grid must be reachable.
    Eq("on-grid max is reachable", 990, ClockStep.SnapWithin(990, s, -1000, 990));
    // Degenerate ranges must not spin or invert.
    Eq("inverted range returns the input", 42, ClockStep.SnapWithin(42, s, 100, -100));

    // ---- memory offset: same idea, 25 MHz grid
    const int m = ClockStep.MemoryMhz;
    Check("memory step is 25", m == 25);
    Eq("memory exact multiple", 800, ClockStep.SnapWithin(800, m, -1000, 4000));
    Eq("memory rounds down", 800, ClockStep.SnapWithin(810, m, -1000, 4000));
    Eq("memory rounds up", 825, ClockStep.SnapWithin(815, m, -1000, 4000));
    Eq("memory zero stays stock", 0, ClockStep.SnapWithin(10, m, -1000, 4000));
    Eq("memory negative symmetric", -825, ClockStep.SnapWithin(-815, m, -1000, 4000));
    Eq("memory top stays on the grid", 4000, ClockStep.SnapWithin(3999, m, -1000, 4000));
    Eq("memory bottom stays on the grid", -1000, ClockStep.SnapWithin(-999, m, -1000, 4000));
    // A range whose ends are NOT multiples of 25 must still come back on-grid.
    Eq("off-grid max steps back", 975, ClockStep.SnapWithin(990, m, -990, 990));

    // An absolute memory clock (AMD) must pass through untouched: its stock value is whatever the
    // driver reports, and rounding it onto a grid would overclock the card just from reading state.
    Eq("absolute clock is never snapped", 2518, ClockStep.SnapWithin(2518, 1, 0, 4000));
}

// ---- BackgroundMode: what the poll costs when nothing is on screen
using (var counting = new CountingBackend())
using (var svc = new TuningService(counting))
{
    svc.Initialize();

    // Foreground: full sample, published and stored.
    svc.StartPolling(20);
    Thread.Sleep(200);
    svc.StopPolling();
    Check("foreground reads full telemetry", counting.FullReads > 0);
    Check("foreground fills history", svc.History.Snapshot().Length > 0);
    Check("foreground sets Latest", svc.Latest != null);

    // Background with no fan curve: nothing to drive, so the driver is not touched at all.
    counting.Reset();
    svc.BackgroundMode = true;
    svc.StartPolling(20);
    Thread.Sleep(200);
    svc.StopPolling();
    Check("background idle: no full reads", counting.FullReads == 0);
    Check("background idle: no temperature reads either", counting.TempReads == 0);

    // Background with a fan curve: temperature only, and the curve still gets stepped.
    counting.Reset();
    int historyBefore = svc.History.Snapshot().Length;
    var bgProfile = new TuningProfile { FanMode = FanMode.Curve };
    bgProfile.FanCurve.Points = new() { new(0, 44), new(120, 44) };   // flat → always 44 %
    svc.Apply(bgProfile);
    counting.Reset();                       // Apply itself reads state; only count the polls
    svc.StartPolling(20);
    Thread.Sleep(200);
    svc.StopPolling();
    Check("background curve: no full reads", counting.FullReads == 0);
    Check("background curve: temperature read instead", counting.TempReads > 0);
    Check("background curve still drives the fan", counting.LastFanPercent == 44);
    Check("background does not grow history", svc.History.Snapshot().Length == historyBefore);
}

// ---- V/F curve undervolt maths (the part that can't be tested on hardware here)
{
    // A toy stock curve: 700 mV→1500 MHz rising to 1100 mV→2700 MHz, 100 MHz per 50 mV.
    var stock = new List<VfPoint>();
    for (int k = 0; k <= 8; k++)
        stock.Add(new VfPoint((uint)((700 + k * 50) * 1000), (1500 + k * 150) * 1000));
    // index: 0=700mV/1500, 2=800mV/1800, 4=900mV/2100, 8=1100mV/2700

    Eq("curve max voltage", 1_100_000, VfCurve.MaxVoltageUv(stock));

    // Cap at 900 mV, no extra clock: points below untouched, 900 mV and above pinned to 2100 MHz.
    var d = VfCurve.ComputeFlattenDeltas(stock, 900_000);
    Eq("flatten: below cap untouched (700mV)", 0, d[0]);
    Eq("flatten: below cap untouched (850mV)", 0, d[3]);
    Eq("flatten: at cap unchanged", 0, d[4]);
    Eq("flatten: 950mV clamped down 150MHz", -150_000, d[5]);
    Eq("flatten: 1100mV clamped down 600MHz", -600_000, d[8]);

    // Resulting effective curve really is flat above the cap.
    var eff = new List<VfPoint>();
    for (int k = 0; k < stock.Count; k++) eff.Add(new VfPoint(stock[k].VoltageUv, stock[k].FrequencyKhz + d[k]));
    Check("flatten: all points >= cap share one frequency",
        eff.Skip(4).Select(p => p.FrequencyKhz).Distinct().Count() == 1);
    Eq("flatten: plateau sits at the cap frequency", 2_100_000, eff[8].FrequencyKhz);

    // The inference used to report real state back to the UI.
    Eq("infer cap from flattened curve", 900_000, VfCurve.InferCapVoltageUv(eff));
    Eq("infer cap from stock curve = none", 0, VfCurve.InferCapVoltageUv(stock));

    // A stock curve that merely flattens out at the top must NOT read as an applied undervolt —
    // a real 4070 Ti has two adjacent 2820 MHz points and was being misreported as a 1085 mV cap.
    var naturalTail = new List<VfPoint>();
    for (int k = 0; k <= 8; k++)
        naturalTail.Add(new VfPoint((uint)((700 + k * 50) * 1000), (1500 + k * 150) * 1000));
    naturalTail[^1] = new VfPoint(naturalTail[^1].VoltageUv, naturalTail[^2].FrequencyKhz);
    Eq("natural top-of-curve flat spot is not a cap", 0, VfCurve.InferCapVoltageUv(naturalTail));

    // Extra clock at the cap point lifts the whole plateau.
    var d2 = VfCurve.ComputeFlattenDeltas(stock, 900_000, extraClockKhz: 100_000);
    Eq("flatten+offset: cap point lifted", 100_000, d2[4]);
    Eq("flatten+offset: top clamped to lifted target", -500_000, d2[8]);

    // Driver delta range is respected.
    var d3 = VfCurve.ComputeFlattenDeltas(stock, 900_000, 0, rangeMinKhz: -200_000, rangeMaxKhz: 200_000);
    Eq("flatten: clamped to driver min delta", -200_000, d3[8]);

    // Cap of 0 (feature off) must produce an all-zero table, never a partial edit.
    Check("cap 0 = no deltas", VfCurve.ComputeFlattenDeltas(stock, 0).All(x => x == 0));

    // Cap below the whole curve falls back to the lowest point instead of writing nonsense.
    var d4 = VfCurve.ComputeFlattenDeltas(stock, 500_000);
    Eq("cap below curve: lowest point kept", 0, d4[0]);
    Eq("cap below curve: top clamped to lowest freq", 1_500_000 - 2_700_000, d4[8]);

    // Explicit per-point targets (graphical curve editor).
    var targets = new Dictionary<int, int> { { 4, 2_200_000 }, { 6, 2_300_000 } };
    var dt = VfCurve.ComputeTargetDeltas(stock, targets);
    Eq("target: index 4 delta = target-stock", 2_200_000 - 2_100_000, dt[4]);
    Eq("target: index 6 delta", 2_300_000 - 2_400_000, dt[6]);
    Eq("target: untouched index stays 0", 0, dt[0]);
    Eq("target: untouched index stays 0 (7)", 0, dt[7]);
    var dtClamp = VfCurve.ComputeTargetDeltas(stock, new Dictionary<int, int> { { 8, 9_000_000 } }, -200_000, 200_000);
    Eq("target: clamped to driver max", 200_000, dtClamp[8]);
    var dtOob = VfCurve.ComputeTargetDeltas(stock, new Dictionary<int, int> { { 99, 5_000_000 }, { -1, 5_000_000 } });
    Check("target: out-of-range indices ignored", dtOob.All(x => x == 0));

    // Invalid/empty slots (voltage or freq 0) are ignored, not treated as real points.
    var sparse = new List<VfPoint> { new(0, 0), new(800_000, 1_800_000), new(0, 0), new(1_000_000, 2_400_000) };
    var d5 = VfCurve.ComputeFlattenDeltas(sparse, 800_000);
    Eq("sparse: empty slot gets no delta", 0, d5[0]);
    Eq("sparse: empty slot 2 gets no delta", 0, d5[2]);
    Eq("sparse: real point clamped", 1_800_000 - 2_400_000, d5[3]);
    Check("empty curve is safe", VfCurve.ComputeFlattenDeltas(new List<VfPoint>(), 900_000).Length == 0);
}

// ---- VoltagePlan: one absolute target -> the two mutually-exclusive levers
{
    const int stock = 1090, max = 1150;      // 4070 Ti: curve top 1090, +60 mV of boost headroom

    Check("at stock = no levers pulled", VoltagePlan.Compute(stock, stock, max) == (0, 0));
    Check("above stock -> boost only", VoltagePlan.Compute(1150, stock, max) == (100, 0));
    Check("half the headroom -> 50%", VoltagePlan.Compute(1120, stock, max) == (50, 0));
    Check("below stock -> flatten only", VoltagePlan.Compute(1000, stock, max) == (0, -90));
    Check("deep undervolt", VoltagePlan.Compute(900, stock, max) == (0, -190));
    Check("boost clamped to 100", VoltagePlan.Compute(9999, stock, max) == (100, 0));
    Check("no headroom -> no boost", VoltagePlan.Compute(1200, stock, stock) == (0, 0));
    Check("unknown curve -> no-op", VoltagePlan.Compute(1000, 0, 0) == (0, 0));
    Check("zero target -> no-op", VoltagePlan.Compute(0, stock, max) == (0, 0));

    // ---- CeilingMv keeps the two baselines apart.
    //
    // The 5070 Ti case that drew "card tops out at 795 mV" next to a "capped 1000 mV" marker on the
    // same graph: the V/F table runs to 1240 mV, the card is only ever seen to reach 1035, and a
    // 1000 mV cap is reported as an offset of 1000-1240 = -240 against the table. Subtracting that
    // from the measured ceiling gave 795. The cap is 1000, so the ceiling is 1000.
    {
        const int table = 1240, seen = 1035, boosted = 1100;
        Check("flatten reported against the table top resolves to the cap",
              VoltagePlan.CeilingMv(0, -240, seen, boosted, table) == 1000);
        Check("no flatten -> the measured ceiling, not the table top",
              VoltagePlan.CeilingMv(0, 0, seen, boosted, table) == seen);
        Check("boost raises the ceiling", VoltagePlan.CeilingMv(100, 0, seen, boosted, table) == boosted);
        // A flatten that lands above the card's measured roof cannot raise it. This is also what
        // makes the result stable when the curve reader returns the shorter table and the offset
        // comes back measured against 1035 instead of 1240.
        Check("a flatten above the roof does not raise it",
              VoltagePlan.CeilingMv(0, -35, seen, boosted, table) == seen);
        Check("flatten still caps a boosted card",
              VoltagePlan.CeilingMv(100, -300, seen, boosted, table) == 940);
        Check("unknown table top -> ceiling unchanged",
              VoltagePlan.CeilingMv(0, -240, seen, boosted, 0) == seen);
        // The old expression, kept as a guard: this is the arithmetic that produced 795.
        Check("the mixed-baseline result is no longer produced",
              VoltagePlan.CeilingMv(0, -240, seen, boosted, table) != VoltagePlan.ToTargetMv(0, -240, seen, boosted));
    }

    // Boost and flatten are never requested together — that is the whole point of one slider.
    foreach (int t in new[] { 850, 900, 1000, 1089, 1090, 1091, 1120, 1150 })
    {
        var (b, o) = VoltagePlan.Compute(t, stock, max);
        Check($"levers exclusive at {t} mV", b == 0 || o == 0);
    }

    // Round trip: seeding the slider from live state should land back on the same target.
    Eq("round trip stock", stock, VoltagePlan.ToTargetMv(0, 0, stock, max));
    Eq("round trip undervolt", 1000, VoltagePlan.ToTargetMv(0, -90, stock, max));
    Eq("round trip full boost", 1150, VoltagePlan.ToTargetMv(100, 0, stock, max));
}

// ---- Stock ceiling tracking, and the voltage levers end to end through the service.
// Regression: a boosted card reports voltages well above its curve top. Folding those into the
// "stock ceiling" pushed it to the top of the slider, after which every target looked like "at or
// above stock" — so Apply computed neither a boost nor a cap, wrote nothing, and reported success.
{
    TuningService NewSvc()
    {
        var s = new TuningService(new MockBackend());
        s.Initialize();
        return s;
    }

    var svc = NewSvc();
    int curveTop = svc.Capabilities.StockMaxVoltageMv;      // 1100 on the mock
    int maxMv = svc.Capabilities.MaxVoltageMv;              // 1160
    Check("ceiling starts at the curve top", svc.StockCeilingMv == curveTop);

    // The card really uses a VID a step above the curve's last point; that much is trusted.
    svc.NoteVoltageObservation(curveTop + 10);
    Check("ceiling follows a nearby observation", svc.StockCeilingMv == curveTop + 10);

    // Anything far above the curve top was reached because of a boost, not because stock sits there.
    svc.NoteVoltageObservation(maxMv);
    Check("ceiling ignores a boosted observation", svc.StockCeilingMv == curveTop + 10);

    // ...and once a lever is engaged, readings stop counting at all.
    var boosted = NewSvc();
    boosted.Apply(new TuningProfile { TargetVoltageMv = maxMv, PowerLimitPercent = 100, TempLimitC = 83 });
    boosted.NoteVoltageObservation(maxMv);
    Check("no ceiling drift while boosting", boosted.StockCeilingMv == curveTop);

    // Full boost must still be reachable after a boost has already been applied once.
    var again = boosted.Apply(new TuningProfile { TargetVoltageMv = maxMv, PowerLimitPercent = 100, TempLimitC = 83 });
    Check("second boost has no errors", again.Count == 0);
    Check("second boost still boosts", boosted.Backend.ReadTuningState(0).VoltageBoostPercent == 100);
    Check("boost leaves no voltage lock", boosted.Backend.ReadVoltageLockMv(0) == 0);

    // A cap below the ceiling goes in as an absolute lock, and clears again.
    var capped = NewSvc();
    var errs = capped.Apply(new TuningProfile { TargetVoltageMv = 950, PowerLimitPercent = 100, TempLimitC = 83 });
    Check("cap applies cleanly", errs.Count == 0);
    Check("cap becomes a 950 mV lock", capped.Backend.ReadVoltageLockMv(0) == 950);
    capped.Apply(new TuningProfile { TargetVoltageMv = curveTop, PowerLimitPercent = 100, TempLimitC = 83 });
    Check("back to stock clears the lock", capped.Backend.ReadVoltageLockMv(0) == 0);

    // ---- boost and cap are independent levers (Afterburner's split), not one derived from the other
    {
        // Boost alone: percentage goes straight through, nothing is capped.
        var b = NewSvc();
        b.Apply(new TuningProfile { VoltageBoostPercent = 50, PowerLimitPercent = 100, TempLimitC = 83 });
        Check("boost alone sets the percentage", b.Backend.ReadTuningState(0).VoltageBoostPercent == 50);
        Check("boost alone leaves no cap", b.Backend.ReadVoltageLockMv(0) == 0);

        // Cap alone: lock goes in, boost stays at stock.
        var c = NewSvc();
        c.Apply(new TuningProfile { TargetVoltageMv = 900, PowerLimitPercent = 100, TempLimitC = 83 });
        Check("cap alone locks at 900", c.Backend.ReadVoltageLockMv(0) == 900);
        Check("cap alone does not boost", c.Backend.ReadTuningState(0).VoltageBoostPercent == 0);

        // Both at once: a card can carry headroom and still be held under load. Neither is inferred
        // from the other, so both must survive the same Apply.
        var both = NewSvc();
        both.Apply(new TuningProfile { VoltageBoostPercent = 40, TargetVoltageMv = 950, PowerLimitPercent = 100, TempLimitC = 83 });
        Check("both: boost kept", both.Backend.ReadTuningState(0).VoltageBoostPercent == 40);
        Check("both: cap kept", both.Backend.ReadVoltageLockMv(0) == 950);

        // An explicit boost is never overwritten by the legacy inference, even with a high target.
        var expl = NewSvc();
        expl.Apply(new TuningProfile { VoltageBoostPercent = 25, TargetVoltageMv = maxMv, PowerLimitPercent = 100, TempLimitC = 83 });
        Check("explicit boost wins over a high target", expl.Backend.ReadTuningState(0).VoltageBoostPercent == 25);

        // Legacy profile: boost was encoded in a target above the ceiling, and must still apply.
        var legacy = NewSvc();
        legacy.Apply(new TuningProfile { TargetVoltageMv = maxMv, PowerLimitPercent = 100, TempLimitC = 83 });
        Check("legacy target above ceiling still boosts", legacy.Backend.ReadTuningState(0).VoltageBoostPercent == 100);
        Check("legacy boost sets no cap", legacy.Backend.ReadVoltageLockMv(0) == 0);

        // Reading back: a pure boost must not come back looking like a cap above the ceiling.
        var read = NewSvc();
        read.Apply(new TuningProfile { VoltageBoostPercent = 60, PowerLimitPercent = 100, TempLimitC = 83 });
        var readBack = read.ReadCurrentAsProfile();
        Check("read back: boost preserved", readBack.VoltageBoostPercent == 60);
        Check("read back: no phantom cap", readBack.TargetVoltageMv == 0);
    }

    // An unreadable V/F curve must not silently disable both levers.
    Check("unknown curve falls back to the slider top",
          VoltagePlan.Compute(950, maxMv, maxMv) == (0, 950 - maxMv));

    // Ordering. The curve-offset and core-offset writes share storage with the cap on real hardware
    // and have cleared it as a side effect, so the cap must be the last thing written. This one is
    // about call order, not the mock's state — the mock is too polite to reproduce the clobber.
    var mock = new MockBackend();
    var ordered = new TuningService(mock);
    ordered.Initialize();
    mock.Calls.Clear();
    ordered.Apply(new TuningProfile { TargetVoltageMv = 950, CoreOffsetMhz = 30, PowerLimitPercent = 100, TempLimitC = 83 });
    int iLock = mock.Calls.LastIndexOf(nameof(MockBackend.SetVoltageLock));
    int iCurve = mock.Calls.LastIndexOf(nameof(MockBackend.SetVoltageCurveOffset));
    int iCore = mock.Calls.LastIndexOf(nameof(MockBackend.SetCoreOffset));
    Check($"cap written after curve offset ({iLock} > {iCurve})", iLock > iCurve && iCurve >= 0);
    Check($"cap written after core offset ({iLock} > {iCore})", iLock > iCore && iCore >= 0);
    Check("cap survives the whole apply", mock.ReadVoltageLockMv(0) == 950);
}

// ---- AMD-style card: offset voltage, absolute memory clock, hardware fan curve, zero RPM.
// The vendor branching in Apply is easy to break silently, so it is pinned here.
{
    var amd = new OffsetStyleBackend();
    var svc = new TuningService(amd);
    svc.Initialize();

    Check("offset style detected", svc.Capabilities.VoltageStyle == VoltageControlStyle.Offset);
    Check("memory slider is absolute", svc.Capabilities.MemoryClockIsAbsolute);

    var errs = svc.Apply(new TuningProfile
    {
        VoltageOffsetMv = -120,
        CoreOffsetMhz = 250,
        MemoryOffsetMhz = 2800,
        PowerLimitPercent = 5,
        ZeroRpm = false,
        MemoryTimingLevel = 1,
        FanMode = FanMode.Curve
    });

    Check($"amd apply is clean ({string.Join(" | ", errs)})", errs.Count == 0);
    Check("voltage offset written", amd.VoltageOffsetMv == -120);
    Check("no voltage lock attempted", !amd.Calls.Contains("SetVoltageLock"));
    Check("no voltage boost attempted", !amd.Calls.Contains("SetVoltageBoost"));
    Check("core offset written", amd.CoreOffsetMhz == 250);
    Check("absolute memory clock written", amd.MemoryMhz == 2800);
    Check("power offset written", amd.PowerPercent == 5);
    Check("zero rpm written", amd.ZeroRpm == false);
    Check("memory timing written", amd.MemoryTiming == 1);
    Check("hardware fan curve used", amd.Calls.Contains("SetFanCurve"));
    Check("software fan loop not engaged", !amd.Calls.Contains("SetFanSpeed"));

    // "Stock" on an absolute memory slider is the rated clock, not zero.
    var stock = TuningProfile.Stock(svc.Capabilities, "Test");
    Check($"stock memory = rated clock ({stock.MemoryOffsetMhz})", stock.MemoryOffsetMhz == 2518);
    Check("stock zero rpm follows the card default", stock.ZeroRpm);
}

// ---- AMD fan mode inference
// The OD8 table is the only fan mechanism on this driver, so ReadTuningState has to work the mode
// back out of it. Pure and array-based, so it runs on the Linux CI job with no driver present.
{
    int[] defT = { 30, 50, 64, 79, 88 }, defS = { 30, 42, 54, 66, 90 };

    var auto = AdlBackend.InferFanMode(defT, defS, defT, defS);
    Check("untouched table reads as auto", auto.Mode == FanMode.Auto && auto.Percent == 0);

    // What SetFanSpeed writes: five equal duties spread across the temperature window.
    var fixedFan = AdlBackend.InferFanMode(
        new[] { 25, 43, 62, 81, 100 }, new[] { 50, 50, 50, 50, 50 }, defT, defS);
    Check("flat table reads as fixed 50%", fixedFan.Mode == FanMode.Fixed && fixedFan.Percent == 50);

    var rising = AdlBackend.InferFanMode(
        new[] { 25, 43, 62, 81, 100 }, new[] { 30, 40, 55, 70, 100 }, defT, defS);
    Check("rising table reads as a curve", rising.Mode == FanMode.Curve);

    // Default duties at non-default temperatures is still a user table, not auto.
    var movedTemps = AdlBackend.InferFanMode(new[] { 25, 45, 60, 75, 95 }, defS, defT, defS);
    Check("shifted temperatures are not auto", movedTemps.Mode == FanMode.Curve);

    // A card exposing no fan points at all must not be reported as manual.
    var none = AdlBackend.InferFanMode(Array.Empty<int>(), Array.Empty<int>(), defT, defS);
    Check("no fan points reads as auto", none.Mode == FanMode.Auto);

    // Regression: this path used to be hard-coded to "not manual", so a fixed duty read back as auto.
    Check("fixed duty is not reported as auto", fixedFan.Mode != FanMode.Auto);
}

// ---- ReleaseFanControl respects who owns the curve
// A software curve dies with the process, so it must be handed back. A driver-owned one does not,
// and resetting it on exit discarded whatever the user (or a startup profile) had just set.
{
    var hw = new OffsetStyleBackend();
    using (var svcHw = new TuningService(hw))
    {
        svcHw.Initialize();
        Check("test backend really is hardware-curve", svcHw.Capabilities.FanCurveIsHardware);
        svcHw.ReleaseFanControl();
        Check("driver-owned fan table survives exit", !hw.Calls.Contains("SetFanAuto"));
    }

    var soft = new MockBackend();
    using (var svcSoft = new TuningService(soft))
    {
        svcSoft.Initialize();
        Check("mock backend enforces curves in software", !svcSoft.Capabilities.FanCurveIsHardware);
        soft.SetFanSpeed(0, -1, 70);
        Check("mock fan is manual once a duty is set", soft.ReadTuningState(0).FanManual);
        svcSoft.ReleaseFanControl();
        Check("software fan control is still handed back on exit", !soft.ReadTuningState(0).FanManual);
    }
}

Console.WriteLine($"{pass} passed, {fail} failed");
return fail == 0 ? 0 : 1;

/// <summary>Minimal AMD-shaped backend: records what Apply asked the driver to do.</summary>
sealed class OffsetStyleBackend : GpuTuner.Core.Backends.IGpuBackend
{
    public List<string> Calls { get; } = new();
    public int VoltageOffsetMv, CoreOffsetMhz, MemoryMhz = 2518, PowerPercent;
    public bool ZeroRpm = true;
    public int MemoryTiming;

    public string BackendName => "Offset-style test GPU";
    public void Initialize() { }
    public IReadOnlyList<GpuDevice> Devices { get; } =
        new[] { new GpuDevice(0, "Test RDNA", "AMD", "PCI", "1.0", 16384, "") };

    public GpuCapabilities GetCapabilities(int i) => new()
    {
        CanSetCoreOffset = true, CoreOffsetMinMhz = -500, CoreOffsetMaxMhz = 1000,
        CanSetMemoryOffset = true, MemoryOffsetMinMhz = 2518, MemoryOffsetMaxMhz = 3000,
        MemoryClockIsAbsolute = true, MemoryClockDefaultMhz = 2518,
        CanSetPowerLimit = true, PowerLimitMinPercent = -30, PowerLimitMaxPercent = 10,
        PowerLimitDefaultPercent = 0, PowerLimitIsOffset = true,
        CanSetTempLimit = false,
        VoltageStyle = VoltageControlStyle.Offset, VoltageOffsetMinMv = -200, VoltageOffsetMaxMv = 0,
        CanSetVoltageCurve = false, CanReadVoltage = true,
        CanSetZeroRpm = true, ZeroRpmDefault = true,
        CanSetMemoryTiming = true, MemoryTimingOptions = new[] { "Default", "Fast timing" },
        CanSetFanSpeed = true, FanCurveIsHardware = true, FanCurvePoints = 5,
        FanMinPercent = 30, FanMaxPercent = 100, FanCount = 1
    };

    public GpuTelemetry ReadTelemetry(int i) => new() { CoreClockMhz = 2900, VoltageMv = 900 };
    public GpuTuningState ReadTuningState(int i) => new()
    {
        CoreOffsetMhz = CoreOffsetMhz, MemoryOffsetMhz = MemoryMhz, PowerLimitPercent = PowerPercent,
        VoltageOffsetMv = VoltageOffsetMv, ZeroRpm = ZeroRpm, MemoryTimingLevel = MemoryTiming
    };

    public void SetCoreOffset(int i, int mhz) { Calls.Add("SetCoreOffset"); CoreOffsetMhz = mhz; }
    public void SetMemoryOffset(int i, int mhz) { Calls.Add("SetMemoryOffset"); MemoryMhz = mhz; }
    public void SetPowerLimit(int i, int pct) { Calls.Add("SetPowerLimit"); PowerPercent = pct; }
    public void SetTempLimit(int i, int c) { Calls.Add("SetTempLimit"); }
    public void SetVoltageBoost(int i, int pct) { Calls.Add("SetVoltageBoost"); }
    public void SetVoltageCurveOffset(int i, int mv, int extra = 0) { Calls.Add("SetVoltageCurveOffset"); VoltageOffsetMv = mv; }
    public void SetVoltageLock(int i, int mv) { Calls.Add("SetVoltageLock"); }
    public void SetZeroRpm(int i, bool on) { Calls.Add("SetZeroRpm"); ZeroRpm = on; }
    public void SetMemoryTiming(int i, int level) { Calls.Add("SetMemoryTiming"); MemoryTiming = level; }
    public void SetFanCurve(int i, FanCurve c) { Calls.Add("SetFanCurve"); }
    public void SetFanSpeed(int i, int fan, int pct) { Calls.Add("SetFanSpeed"); }
    public void SetFanAuto(int i) { Calls.Add("SetFanAuto"); }
    public void ResetToDefaults(int i) { Calls.Add("ResetToDefaults"); }
    public void Dispose() { }
}

/// <summary>
/// Wraps the mock and counts which telemetry path the poll loop took, so BackgroundMode can be
/// tested for what it actually costs rather than just what it returns.
/// </summary>
sealed class CountingBackend : GpuTuner.Core.Backends.IGpuBackend
{
    private readonly MockBackend _inner = new();

    public int FullReads;
    public int TempReads;
    public int LastFanPercent = -1;

    public void Reset() { FullReads = 0; TempReads = 0; }

    public GpuTelemetry ReadTelemetry(int i) { FullReads++; return _inner.ReadTelemetry(i); }
    public GpuTelemetry ReadTemperatureOnly(int i) { TempReads++; return new GpuTelemetry { TemperatureC = 55 }; }

    public string BackendName => "Counting";
    public void Initialize() => _inner.Initialize();
    public IReadOnlyList<GpuDevice> Devices => _inner.Devices;
    public GpuCapabilities GetCapabilities(int i) => _inner.GetCapabilities(i);
    public GpuTuningState ReadTuningState(int i) => _inner.ReadTuningState(i);

    public void SetCoreOffset(int i, int mhz) => _inner.SetCoreOffset(i, mhz);
    public void SetMemoryOffset(int i, int mhz) => _inner.SetMemoryOffset(i, mhz);
    public void SetPowerLimit(int i, int pct) => _inner.SetPowerLimit(i, pct);
    public void SetTempLimit(int i, int c) => _inner.SetTempLimit(i, c);
    public void SetVoltageBoost(int i, int pct) => _inner.SetVoltageBoost(i, pct);
    public void SetVoltageCurveOffset(int i, int mv, int extra = 0) => _inner.SetVoltageCurveOffset(i, mv, extra);
    public void SetFanSpeed(int i, int fan, int pct) { LastFanPercent = pct; _inner.SetFanSpeed(i, fan, pct); }
    public void SetFanAuto(int i) => _inner.SetFanAuto(i);
    public void ResetToDefaults(int i) => _inner.ResetToDefaults(i);
    public void Dispose() => _inner.Dispose();
}
