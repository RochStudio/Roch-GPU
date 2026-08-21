using System;
using System.IO;
using System.Linq;
using System.Windows;
using System.Windows.Threading;
using GpuTuner.Core.Backends;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Backends.Nvidia;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

namespace GpuTuner.App;

/// <summary>
/// Startup flow:
///   GpuTuner.exe                          → GUI
///   GpuTuner.exe --mock                   → GUI on the simulated GPU
///   GpuTuner.exe --apply-profile X --exit → apply saved profile, no window, exit (used by the startup task)
///   GpuTuner.exe --apply-profile X --minimized → apply, then stay resident in the tray (needed for fan curves)
/// </summary>
public partial class App : Application
{
    public static TuningService? Service { get; private set; }
    public static ProfileStore Store { get; } = new ProfileStore();
    public static AppSettings Settings { get; private set; } = new();

    /// <summary>True for the --exit startup-task run: no window, no user, so no modal dialogs.</summary>
    private static bool _headless;
    public static string LogPath => Path.Combine(Store.RootDirectory, "roch-gpu.log");

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        Settings = Store.LoadSettings();

        var args = e.Args.ToList();
        bool mock = args.Contains("--mock") || Settings.UseMockBackend;
        bool exitAfter = args.Contains("--exit");
        _headless = exitAfter;
        bool minimized = args.Contains("--minimized") || Settings.StartMinimized;
        string? applyProfile = null;
        int idx = args.IndexOf("--apply-profile");
        if (idx >= 0 && idx + 1 < args.Count) applyProfile = args[idx + 1];

        DispatcherUnhandledException += OnUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += (_, ex) =>
        {
            LogLine("FATAL: " + ex.ExceptionObject);
            Service?.ReleaseFanControl();
        };

        // NVIDIA or AMD, whichever initialises. One binary, either card.
        IGpuBackend backend = new MockBackend();
        Exception? detectError = null;
        if (!mock)
        {
            try
            {
                backend = BackendFactory.CreateAndInitialize(out var attempts);
                foreach (var a in attempts) LogLine($"Backend probe — {a.Name}: {a.Error}");
            }
            // Not "e": OnStartup's own StartupEventArgs parameter is already named that.
            catch (GpuBackendException probeFailure) { detectError = probeFailure; }
        }

        Service = new TuningService(backend, Settings.HistorySeconds);
        Service.Log += LogLine;

        try
        {
            if (detectError != null) throw detectError;
            Service.Initialize(0);
        }
        catch (GpuBackendException ex)
        {
            LogLine("Init failed: " + ex.Message);
            if (!mock)
            {
                var r = MessageBox.Show(
                    ex.Message + "\n\nStart with a simulated GPU instead (UI demo only)?",
                    "ROCH GPU — no supported GPU", MessageBoxButton.YesNo, MessageBoxImage.Warning);
                if (r == MessageBoxResult.Yes)
                {
                    Service.Dispose();
                    Service = new TuningService(new MockBackend(), Settings.HistorySeconds);
                    Service.Log += LogLine;
                    Service.Initialize(0);
                }
                else { Shutdown(1); return; }
            }
            else { Shutdown(1); return; }
        }

        if (applyProfile != null)
        {
            TuningProfile? p = null;
            try { p = Store.Load(applyProfile); }
            catch (Exception ex) { LogLine($"Profile '{applyProfile}' could not be read: {ex.Message}"); }
            if (p == null)
            {
                LogLine($"Startup profile '{applyProfile}' not found.");
            }
            else
            {
                var errs = Service.Apply(p);
                LogLine(errs.Count == 0 ? $"Startup profile '{applyProfile}' applied." : "Startup apply errors: " + string.Join("; ", errs));
            }
            if (exitAfter)
            {
                // Fan curve can't survive without a resident process; the CLI/startup docs say so.
                Service.Dispose();
                Shutdown(0);
                return;
            }
        }

        var win = new MainWindow(Service, minimized);
        MainWindow = win;
        win.Show();
        if (minimized) win.MinimizeToTray();
    }

    private void OnUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        LogLine("Unhandled: " + e.Exception);
        // A message box in the logon-task run would wait forever on a desktop nobody is looking at.
        if (!_headless)
            MessageBox.Show(e.Exception.Message, "ROCH GPU error", MessageBoxButton.OK, MessageBoxImage.Error);
        e.Handled = true;
        // ShutdownMode is OnExplicitShutdown: if the crash happened before the main window existed
        // (e.g. inside OnStartup), swallowing it would leave a windowless zombie process behind.
        if (MainWindow == null) Shutdown(1);
    }

    protected override void OnExit(ExitEventArgs e)
    {
        try
        {
            // If a fan curve or fixed fan was active, hand fans back to the driver on the way out.
            Service?.ReleaseFanControl();
            Service?.Dispose();
            Store.SaveSettings(Settings);
        }
        catch { }
        base.OnExit(e);
    }

    private const long MaxLogBytes = 1024 * 1024;

    public static void LogLine(string s)
    {
        try
        {
            // Keep one previous log and cap the live one: a driver that fails every poll would
            // otherwise fill the disk overnight.
            var info = new FileInfo(LogPath);
            if (info.Exists && info.Length > MaxLogBytes)
            {
                string old = LogPath + ".old";
                File.Delete(old);
                File.Move(LogPath, old);
            }
            File.AppendAllText(LogPath, $"{DateTime.Now:yyyy-MM-dd HH:mm:ss}  {s}{Environment.NewLine}");
        }
        catch { }
    }
}
