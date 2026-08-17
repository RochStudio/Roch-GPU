using GpuTuner.Core.Backends;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Backends.Nvidia;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

// rochoc — headless companion to the GUI. Same engine, no window.
//
//   rochoc info                       show GPUs, capabilities, current tuning
//   rochoc monitor [--interval 1000]  live telemetry until Ctrl+C
//   rochoc apply [--gpu 0] [--core +150] [--mem +800] [--power 90] [--temp 80] [--fan 60|auto]
//   rochoc apply-profile <name> [--gpu 0]     apply a saved GUI profile
//   rochoc list-profiles
//   rochoc reset [--gpu 0]
//   rochoc startup --enable <profile> | --disable | --status
//   add --mock anywhere to use the simulated GPU

return Cli.Run(args);

static class Cli
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

            switch (args[0].ToLowerInvariant())
            {
                case "info": svc.Initialize(gpu); return Info(svc);
                case "monitor": svc.Initialize(gpu); return Monitor(svc, opts);
                case "apply": svc.Initialize(gpu); return Apply(svc, opts);
                case "apply-profile":
                    if (args.Length < 2) { Console.Error.WriteLine("profile name required"); return 2; }
                    svc.Initialize(gpu); return ApplyProfile(svc, store, args[1]);
                case "list-profiles":
                    foreach (var n in store.ListProfileNames()) Console.WriteLine(n);
                    return 0;
                case "reset": svc.Initialize(gpu); svc.ResetToDefaults(); Console.WriteLine("Reset to defaults."); return 0;
                case "diag":
                    svc.Initialize(gpu);
                    var dump = svc.Backend.GetDiagnostics(gpu);
                    Console.WriteLine(dump);
                    try
                    {
                        var path = Path.Combine(Directory.GetCurrentDirectory(), "rochoc-diag.txt");
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
            Console.Error.WriteLine("No GPU at that --gpu index. Run 'rochoc info' to list what was found.");
            return 2;
        }
        catch (GpuBackendException e) { Console.Error.WriteLine("ERROR: " + e.Message); return 1; }
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
        Console.WriteLine($"Undervolt     : {(c.CanSetVoltageCurve ? $"{c.VoltageOffsetMinMv}..0 mV (stock max {c.StockMaxVoltageMv} mV)" : "not supported")}");
        Console.WriteLine($"Fans          : {(c.CanSetFanSpeed ? $"{c.FanCount} fan(s), {c.FanMinPercent}..{c.FanMaxPercent} %" : "not supported")}");
        var s = svc.Backend.ReadTuningState(svc.GpuIndex);
        Console.WriteLine();
        Console.WriteLine($"Current: core {s.CoreOffsetMhz:+#;-#;0} MHz, mem {s.MemoryOffsetMhz:+#;-#;0} MHz, power {s.PowerLimitPercent}%, temp {s.TempLimitC}°C, vboost {s.VoltageBoostPercent}%, uv {s.VoltageOffsetMv} mV, fan {(s.FanManual ? s.FanPercent + "% manual" : "auto")}");
        var t = svc.Backend.ReadTelemetry(svc.GpuIndex);
        Console.WriteLine(Fmt(t));
        return 0;
    }

    static int Monitor(TuningService svc, Dictionary<string, string> o)
    {
        int interval = o.TryGetValue("interval", out var s) && int.TryParse(s, out var i) ? i : 1000;
        Console.WriteLine("Ctrl+C to stop.");
        var done = new ManualResetEventSlim();
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; done.Set(); };
        svc.TelemetryUpdated += t => Console.WriteLine(Fmt(t));
        svc.StartPolling(interval);
        done.Wait();
        return 0;
    }

    static int Apply(TuningService svc, Dictionary<string, string> o)
    {
        var p = svc.ReadCurrentAsProfile();
        if (o.TryGetValue("core", out var core)) p.CoreOffsetMhz = int.Parse(core);
        if (o.TryGetValue("mem", out var mem)) p.MemoryOffsetMhz = int.Parse(mem);
        if (o.TryGetValue("power", out var pw)) p.PowerLimitPercent = int.Parse(pw);
        if (o.TryGetValue("temp", out var tp)) p.TempLimitC = int.Parse(tp);
        if (o.TryGetValue("volt", out var vb)) p.VoltageBoostPercent = int.Parse(vb);
        if (o.TryGetValue("uv", out var uv)) p.VoltageOffsetMv = int.Parse(uv);
        if (o.TryGetValue("fan", out var fan))
        {
            if (fan.Equals("auto", StringComparison.OrdinalIgnoreCase)) p.FanMode = FanMode.Auto;
            else { p.FanMode = FanMode.Fixed; p.FixedFanPercent = int.Parse(fan); }
        }
        return Report(svc.Apply(p));
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
        if (errs.Count == 0) { Console.WriteLine("Applied."); return 0; }
        foreach (var e in errs) Console.Error.WriteLine(e);
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
            StartupTaskService.Register(Path.ChangeExtension(exe, ".exe"), prof, stayResident: false);
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
        rochoc — Roch GPU OC command line / undervolt CLI (needs admin for writes)

          rochoc info
          rochoc monitor [--interval 1000]
          rochoc apply [--gpu 0] [--core +150] [--mem +800] [--power 90] [--temp 80] [--volt 25] [--uv -100] [--fan 60|auto]
          rochoc apply-profile <name> [--gpu 0]
          rochoc list-profiles
          rochoc reset [--gpu 0]
          rochoc diag                       dump everything the driver reports (for bug reports)
          rochoc startup --enable <profile> | --disable | --status
          (add --mock to any command to use a simulated GPU)
        """);
}
