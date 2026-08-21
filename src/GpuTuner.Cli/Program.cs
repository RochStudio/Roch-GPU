using GpuTuner.Core.Backends;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Backends.Nvidia;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

// "ROCH GPU.exe" — headless companion to the GUI. Same engine, no window.
//
//   "ROCH GPU.exe" info                       show GPUs, capabilities, current tuning
//   "ROCH GPU.exe" monitor [--interval 1000]  live telemetry until Ctrl+C
//   "ROCH GPU.exe" apply [--gpu 0] [--core +150] [--mem +800] [--power 90] [--temp 80] [--fan 60|auto]
//   "ROCH GPU.exe" apply-profile <name> [--gpu 0]     apply a saved GUI profile
//   "ROCH GPU.exe" list-profiles
//   "ROCH GPU.exe" reset [--gpu 0]
//   "ROCH GPU.exe" startup --enable <profile> | --disable | --status
//   add --mock anywhere to use the simulated GPU

namespace GpuTuner.Cli;

/// <summary>The command-line half of the one executable. See <c>EntryPoint</c> for which half runs.</summary>
public static class CommandLine
{
    public static int Run(string[] args)
    {
        if (args.Length == 0 || args[0] is "-h" or "--help" or "help") { Usage(); return 0; }
        var opts = Parse(args.Skip(1));
        bool mock = opts.ContainsKey("mock");
        int gpu = opts.TryGetValue("gpu", out var gs) && int.TryParse(gs, out var gi) ? gi : 0;

        try
        {
            IGpuBackend backend = mock
                ? new MockBackend()
                : BackendFactory.CreateAndInitialize(out _);
            using var svc = new TuningService(backend);
            var store = new ProfileStore();

            // Every command that reasons about voltage needs the ceilings an earlier run measured;
            // without them an undervolt offset is taken from the V/F table's top rather than from
            // where this card actually runs, which can be tens of millivolts out.
            void Start()
            {
                svc.Initialize(gpu);
                var s = store.LoadSettings();
                string name = svc.Backend.Devices[svc.GpuIndex].Name;
                if (s.ObservedMaxVoltageByGpu.TryGetValue(name, out var seen)) svc.SeedObservedMaxVoltage(seen);
                if (s.ObservedMaxBoostedVoltageByGpu.TryGetValue(name, out var seenB)) svc.SeedObservedMaxBoostedVoltage(seenB);
                if (SeedRailDefaults(svc, s, name)) store.SaveSettings(s);
            }

            switch (args[0].ToLowerInvariant())
            {
                case "info": Start(); return Info(svc);
                case "monitor": Start(); return Monitor(svc, store, opts);
                case "apply": Start(); return Apply(svc, opts);
                case "apply-profile":
                    if (args.Length < 2) { Console.Error.WriteLine("profile name required"); return 2; }
                    Start(); return ApplyProfile(svc, store, args[1]);
                case "list-profiles":
                    foreach (var n in store.ListProfileNames()) Console.WriteLine(n);
                    return 0;
                case "reset": Start(); svc.ResetToDefaults(); Console.WriteLine("Reset to defaults."); return 0;
                case "diag":
                    svc.Initialize(gpu);
                    var dump = svc.Backend.GetDiagnostics(gpu);
                    Console.WriteLine(dump);
                    try
                    {
                        var path = Path.Combine(Directory.GetCurrentDirectory(), "roch-gpu-diag.txt");
                        File.WriteAllText(path, dump);
                        Console.WriteLine($"(also written to {path})");
                    }
                    catch { }
                    return 0;
                case "startup": return Startup(opts);
                default: Usage(); return 2;
            }
        }
        catch (FormatException)
        {
            Console.Error.WriteLine("A numeric option was not a number. Values are plain integers, e.g. --core 120 --power 110.");
            return 2;
        }
        catch (ArgumentOutOfRangeException)
        {
            Console.Error.WriteLine("No GPU at that --gpu index. Run 'ROCH GPU.exe info' to list what was found.");
            return 2;
        }
        catch (GpuBackendException e) { Console.Error.WriteLine("ERROR: " + e.Message); return 1; }
    }

    /// <summary>
    /// Carry the rail ceilings forward, recording them the first time a GPU is seen. Returns true
    /// when something was recorded and the settings need saving.
    /// </summary>
    static bool SeedRailDefaults(TuningService svc, AppSettings s, string gpuName)
    {
        var c = svc.Capabilities;
        bool dirty = false;
        if (c.CanSetVoltageRail && !s.NvvddDefaultMaxByGpu.ContainsKey(gpuName) && c.VoltageRailStockMaxMv > 0)
        { s.NvvddDefaultMaxByGpu[gpuName] = c.VoltageRailStockMaxMv; dirty = true; }
        if (c.CanSetMsvddRail && !s.MsvddDefaultMaxByGpu.ContainsKey(gpuName) && c.MsvddRailStockMaxMv > 0)
        { s.MsvddDefaultMaxByGpu[gpuName] = c.MsvddRailStockMaxMv; dirty = true; }

        s.NvvddDefaultMaxByGpu.TryGetValue(gpuName, out int nv);
        s.MsvddDefaultMaxByGpu.TryGetValue(gpuName, out int ms);
        svc.SeedRailDefaults(nv, ms);
        return dirty;
    }

    static int Info(TuningService svc)
    {
        foreach (var d in svc.Backend.Devices)
            Console.WriteLine($"[{d.Index}] {d.Name}  {d.VramMegabytes} MB  driver {d.DriverVersion}  vBIOS {d.BiosVersion}  ({d.BusId})");
        var c = svc.Capabilities;
        Console.WriteLine();
        Console.WriteLine($"Core offset   : {(c.CanSetCoreOffset ? $"{c.CoreOffsetMinMhz}..{c.CoreOffsetMaxMhz} MHz" : "not supported")}");
        Console.WriteLine($"Memory offset : {(c.CanSetMemoryOffset ? $"{c.MemoryOffsetMinMhz}..{c.MemoryOffsetMaxMhz} MHz" : "not supported")}");
        Console.WriteLine($"Power limit   : {(c.CanSetPowerLimit ? $"{c.PowerLimitMinPercent}..{c.PowerLimitMaxPercent} % (default {c.PowerLimitDefaultPercent})" : "not supported")}");
        Console.WriteLine($"Temp limit    : {(c.CanSetTempLimit ? $"{c.TempLimitMinC}..{c.TempLimitMaxC} °C (default {c.TempLimitDefaultC})" : "not supported")}");
        Console.WriteLine($"Voltage boost : {(c.CanSetVoltageBoost ? $"{c.VoltageBoostMinPercent}..{c.VoltageBoostMaxPercent} %" : "not supported")}");
        // Two different undervolt mechanisms reach the same slider: NVIDIA flattens the V/F curve,
        // AMD offsets the whole curve and has no editable one. Gating on CanSetVoltageCurve alone
        // reported "not supported" on a card that was actively undervolted (see the uv field below).
        bool canUndervolt = c.CanSetVoltageCurve || c.VoltageStyle == VoltageControlStyle.Offset;
        string undervolt = !canUndervolt ? "not supported"
            : c.CanSetVoltageCurve ? $"{c.VoltageOffsetMinMv}..0 mV (stock max {c.StockMaxVoltageMv} mV)"
            : $"{c.VoltageOffsetMinMv}..{c.VoltageOffsetMaxMv} mV (whole-curve offset)";
        Console.WriteLine($"Undervolt     : {undervolt}");
        Console.WriteLine($"Core rail     : {(c.CanSetVoltageRail ? $"{c.VoltageRailMinMv}..{c.VoltageRailMaxMv} mV ceiling (now {c.VoltageRailStockMaxMv} mV), floor {c.VoltageRailFloorMinMv}..{c.VoltageRailFloorMaxMv} mV (stock {c.VoltageRailStockFloorMv} mV)" : "not supported")}");
        Console.WriteLine($"MSVDD rail    : {(c.CanSetMsvddRail ? $"{c.MsvddRailMinMv}..{c.MsvddRailMaxMv} mV ceiling (now {c.MsvddRailStockMaxMv} mV), floor {c.MsvddRailFloorMinMv}..{c.MsvddRailFloorMaxMv} mV (stock {c.MsvddRailStockFloorMv} mV)" : "not supported")}");
        Console.WriteLine($"Crossbar      : {(c.CanSetXbarOffset ? $"{c.XbarOffsetMinMhz}..{c.XbarOffsetMaxMhz} MHz offset" : "not supported")}");
        Console.WriteLine($"Fans          : {(c.CanSetFanSpeed ? $"{c.FanCount} fan(s), {c.FanMinPercent}..{c.FanMaxPercent} %" : "not supported")}");
        var s = svc.Backend.ReadTuningState(svc.GpuIndex);
        Console.WriteLine();
        // A backend that can read the three fan modes apart says so; the rest only expose a manual
        // flag, through which a hardware curve is indistinguishable from auto.
        string fan = s.DetectedFanMode switch
        {
            FanMode.Fixed => $"{s.FanPercent}% fixed",
            FanMode.Curve => "hardware curve",
            FanMode.Auto => "auto",
            _ => s.FanManual ? $"{s.FanPercent}% manual" : "auto"
        };
        // 0 means the card reported no thermal policy at all, which is not the same as a limit of 0.
        string tempLimit = s.TempLimitC > 0 ? $"{s.TempLimitC}°C" : "n/a";

        // Measure the undervolt from the same ceiling an apply measures it from. The backend reports
        // it against the V/F table's top, which on a card that never reaches that top reads tens of
        // millivolts deeper than what was asked for: --uv -90 came back as -145.
        int uvMv = s.VoltageOffsetMv;
        if (c.VoltageStyle == VoltageControlStyle.Absolute)
        {
            int lockMv = svc.Backend.ReadVoltageLockMv(svc.GpuIndex);
            uvMv = lockMv > 0 && svc.StockCeilingMv > 0 ? lockMv - svc.StockCeilingMv : 0;
        }
        Console.WriteLine($"Current: core {s.CoreOffsetMhz:+#;-#;0} MHz, mem {s.MemoryOffsetMhz:+#;-#;0} MHz, power {s.PowerLimitPercent}%, temp {tempLimit}, vboost {s.VoltageBoostPercent}%, uv {uvMv} mV, rail {(s.VoltageRailMaxMv > 0 ? s.VoltageRailFloorMv + "-" + s.VoltageRailMaxMv + " mV" : "n/a")}, msvdd {(s.MsvddRailMaxMv > 0 ? s.MsvddRailFloorMv + "-" + s.MsvddRailMaxMv + " mV" : "n/a")}, xbar {s.XbarOffsetMhz:+#;-#;0} MHz, fan {fan}");
        var t = svc.Backend.ReadTelemetry(svc.GpuIndex);
        Console.WriteLine(Fmt(t));
        return 0;
    }

    static int Monitor(TuningService svc, ProfileStore store, Dictionary<string, string> o)
    {
        int interval = o.TryGetValue("interval", out var s) && int.TryParse(s, out var i) ? i : 1000;
        Console.WriteLine("Ctrl+C to stop.");

        // The stock-ceiling estimate is only fed while something is polling, and the GUI's monitor is
        // usually closed. Carrying it through the same store the GUI uses means a load run from here
        // teaches both front-ends what the card really tops out at.
        string gpuName = svc.Backend.Devices[svc.GpuIndex].Name;
        var settings = store.LoadSettings();
        if (settings.ObservedMaxVoltageByGpu.TryGetValue(gpuName, out var seenMv))
            svc.SeedObservedMaxVoltage(seenMv);
        if (settings.ObservedMaxBoostedVoltageByGpu.TryGetValue(gpuName, out var seenBoostMv))
            svc.SeedObservedMaxBoostedVoltage(seenBoostMv);
        int recorded = svc.ObservedMaxVoltageMv, recordedBoost = svc.ObservedMaxBoostedVoltageMv;

        var done = new ManualResetEventSlim();
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; done.Set(); };
        svc.TelemetryUpdated += t =>
        {
            Console.WriteLine(Fmt(t));
            bool stockMoved = svc.ObservedMaxVoltageMv > recorded;
            bool boostMoved = svc.ObservedMaxBoostedVoltageMv > recordedBoost;
            if (!stockMoved && !boostMoved) return;

            var cur = store.LoadSettings();
            if (stockMoved)
            {
                recorded = svc.ObservedMaxVoltageMv;
                cur.ObservedMaxVoltageByGpu[gpuName] = recorded;
                Console.WriteLine($"   ceiling: recorded {recorded} mV stock, under load");
            }
            if (boostMoved)
            {
                recordedBoost = svc.ObservedMaxBoostedVoltageMv;
                cur.ObservedMaxBoostedVoltageByGpu[gpuName] = recordedBoost;
                Console.WriteLine($"   ceiling: recorded {recordedBoost} mV boosted, under load");
            }
            store.SaveSettings(cur);
        };
        svc.StartPolling(interval);
        done.Wait();
        return 0;
    }

    static int Apply(TuningService svc, Dictionary<string, string> o)
    {
        var p = svc.ReadCurrentAsProfile();
        var notes = new List<string>();
        if (o.TryGetValue("core", out var core)) p.CoreOffsetMhz = int.Parse(core);
        if (o.TryGetValue("mem", out var mem)) p.MemoryOffsetMhz = int.Parse(mem);
        if (o.TryGetValue("power", out var pw)) p.PowerLimitPercent = int.Parse(pw);
        if (o.TryGetValue("temp", out var tp))
        {
            p.TempLimitC = int.Parse(tp);
            // Here, not in the service: only this side knows the flag was actually typed rather than
            // carried along from the card's current state.
            if (!svc.Capabilities.CanSetTempLimit)
                notes.Add($"{TuningService.NotePrefix} this card exposes no temperature limit — {tp}°C ignored.");
        }
        if (o.TryGetValue("volt", out var vb)) p.VoltageBoostPercent = int.Parse(vb);
        if (o.TryGetValue("uv", out var uv)) p.VoltageOffsetMv = int.Parse(uv);
        if (o.TryGetValue("xbar", out var xb))
        {
            p.XbarOffsetMhz = int.Parse(xb);
            if (!svc.Capabilities.CanSetXbarOffset)
                notes.Add($"{TuningService.NotePrefix} this card exposes no crossbar clock — {xb} MHz ignored.");
        }
        if (o.TryGetValue("nvvdd-min", out var rmin)) p.VoltageRailFloorMv = int.Parse(rmin);
        if (o.TryGetValue("msvdd-min", out var mmin)) p.MsvddRailFloorMv = int.Parse(mmin);
        if (o.TryGetValue("msvdd", out var ms))
        {
            p.MsvddRailMaxMv = int.Parse(ms);
            if (!svc.Capabilities.CanSetMsvddRail)
                notes.Add($"{TuningService.NotePrefix} this card exposes no MSVDD rail - {ms} mV ignored.");
        }
        if (o.TryGetValue("nvvdd", out var rail))
        {
            p.VoltageRailMaxMv = int.Parse(rail);
            if (!svc.Capabilities.CanSetVoltageRail)
                notes.Add($"{TuningService.NotePrefix} this card exposes no core voltage rail — {rail} mV ignored.");
        }
        // Typing one of the gated flags is the arming gesture; the GUI has a button for it. Without
        // any of them the profile carries the gate shut, and the apply puts the rails and crossbar
        // back to the driver's own values rather than leaving an earlier tune half-standing.
        p.XocEnabled = o.ContainsKey("nvvdd") || o.ContainsKey("nvvdd-min")
                    || o.ContainsKey("msvdd") || o.ContainsKey("msvdd-min") || o.ContainsKey("xbar");
        if (p.XocEnabled) notes.Add($"{TuningService.NotePrefix} XOC armed for this apply (rails/crossbar written).");
        if (o.TryGetValue("fan", out var fan))
        {
            if (fan.Equals("auto", StringComparison.OrdinalIgnoreCase)) p.FanMode = FanMode.Auto;
            else { p.FanMode = FanMode.Fixed; p.FixedFanPercent = int.Parse(fan); }
        }
        notes.AddRange(svc.Apply(p));
        return Report(notes);
    }

    static int ApplyProfile(TuningService svc, ProfileStore store, string name)
    {
        var p = store.Load(name);
        if (p == null) { Console.Error.WriteLine($"Profile '{name}' not found in {store.ProfilesDirectory}"); return 2; }
        if (p.FanMode == FanMode.Curve)
            Console.Error.WriteLine("Note: fan curve mode needs a resident process; the CLI applies clocks/limits and leaves fans on auto. Use the GUI with --minimized for curves.");
        var errs = svc.Apply(p);
        return Report(errs);
    }

    static int Report(IReadOnlyList<string> errs)
    {
        // Notes are what the apply did differently, not what it failed to do — they belong on stdout
        // with a success exit code. Only a real failure is stderr and 1.
        foreach (var n in errs.Where(e => e.StartsWith(TuningService.NotePrefix, StringComparison.Ordinal)))
            Console.WriteLine(n);
        var failures = errs.Where(e => !e.StartsWith(TuningService.NotePrefix, StringComparison.Ordinal)).ToList();
        if (failures.Count == 0) { Console.WriteLine("Applied."); return 0; }
        foreach (var e in failures) Console.Error.WriteLine(e);
        return 1;
    }

    static int Startup(Dictionary<string, string> o)
    {
        if (!StartupTaskService.IsWindows) { Console.Error.WriteLine("Windows only."); return 1; }
        if (o.ContainsKey("status")) { Console.WriteLine(StartupTaskService.Exists() ? "enabled" : "disabled"); return 0; }
        if (o.ContainsKey("disable")) { StartupTaskService.Unregister(); Console.WriteLine("Startup task removed."); return 0; }
        if (o.TryGetValue("enable", out var prof))
        {
            var exe = Environment.ProcessPath ?? throw new InvalidOperationException("cannot resolve own path");
            StartupTaskService.Register(Path.ChangeExtension(exe, ".exe"), prof);
            Console.WriteLine($"Startup task registered: apply '{prof}' at logon (elevated).");
            return 0;
        }
        Console.Error.WriteLine("use --enable <profile> | --disable | --status"); return 2;
    }

    static string Fmt(GpuTelemetry t) =>
        $"{t.Timestamp.ToLocalTime():HH:mm:ss}  {t.CoreClockMhz,5:0} MHz core  {t.MemoryClockMhz,6:0} MHz mem  " +
        $"{t.TemperatureC,4:0.#}°C{(double.IsNaN(t.HotSpotC) ? "" : $" hs{t.HotSpotC:0}")}{(double.IsNaN(t.MemoryTemperatureC) ? "" : $" mem{t.MemoryTemperatureC:0}")}  " +
        $"{(double.IsNaN(t.VoltageMv) ? "   -" : $"{t.VoltageMv,4:0} mV")}  " +
        $"{t.PowerPercent,5:0.#}% TDP  load {t.GpuLoadPercent,3:0}%  fan {t.FanPercent,3:0}% ({t.FanRpm:0} rpm)  {t.PerfState} {t.LimitReason}";

    static Dictionary<string, string> Parse(IEnumerable<string> args)
    {
        var d = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        string? key = null;
        foreach (var a in args)
        {
            if (a.StartsWith("--")) { key = a[2..]; d[key] = ""; }
            else if (key != null) { d[key] = a; key = null; }
        }
        return d;
    }

    static void Usage() => Console.WriteLine("""
        "ROCH GPU.exe" — ROCH GPU command line / undervolt CLI (needs admin for writes)

          "ROCH GPU.exe" info
          "ROCH GPU.exe" monitor [--interval 1000]
          "ROCH GPU.exe" apply [--gpu 0] [--core +150] [--mem +800] [--power 90] [--temp 80] [--volt 25] [--uv -100] [--nvvdd 1100] [--nvvdd-min 800] [--msvdd 1050] [--msvdd-min 800] [--xbar +100] [--fan 60|auto]
            the rail and crossbar flags are gated: pass one to arm them, pass none and they go back to driver defaults
          "ROCH GPU.exe" apply-profile <name> [--gpu 0]
          "ROCH GPU.exe" list-profiles
          "ROCH GPU.exe" reset [--gpu 0]
          "ROCH GPU.exe" diag                       dump everything the driver reports (for bug reports)
          "ROCH GPU.exe" startup --enable <profile> | --disable | --status
          (add --mock to any command to use a simulated GPU)
        """);
}
