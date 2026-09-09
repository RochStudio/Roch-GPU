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
///   RochGPU.exe                          → GUI
///   RochGPU.exe --mock                   → GUI on the simulated GPU
///   RochGPU.exe --apply-profile X --exit → apply saved profile, no window, exit (used by the startup task)
///   RochGPU.exe --apply-profile X --minimized → apply, then stay resident in the tray (needed for fan curves)
/// </summary>
public partial class App : Application
{
    public static TuningService? Service { get; private set; }
    public static ProfileStore Store { get; } = new ProfileStore();
    public static AppSettings Settings { get; private set; } = new();
    public static ProfileRecovery? Recovery { get; private set; }
    public static string? RecoveryNotice { get; private set; }

    public static System.Collections.Generic.IReadOnlyList<string> TrialProfile(TuningProfile profile)
    {
        var recovery = Recovery;
        if (recovery == null) return new[] { "Profile recovery is unavailable; no settings applied." };
        var candidate = profile.Clone();
        candidate.ClampTo(Service!.Capabilities);
        try
        {
            var errors = recovery.Begin(candidate);
            if (errors.Count == 0 || TuningService.OnlyNotes(errors))
            {
                recovery.Confirm();
                return errors;
            }
            var rollback = recovery.Revert();
            if (rollback.Count > 0 && !TuningService.OnlyNotes(rollback))
                return new[] { "Recovery failed; retry by reopening Roch GPU: " + string.Join("; ", rollback) };
            return new[] { errors.Count > 0 && !TuningService.OnlyNotes(errors)
                ? "Trial failed and was reverted: " + string.Join("; ", errors)
                : "Trial reverted; startup settings were not changed." };
        }
        catch (Exception ex)
        {
            try
            {
                if (recovery.HasPending)
                {
                    var rollback = recovery.Revert();
                    if (rollback.Count > 0 && !TuningService.OnlyNotes(rollback))
                        return new[] { ex.Message + " Recovery failed: " + string.Join("; ", rollback) };
                }
            }
            catch (Exception rollback) { return new[] { ex.Message + " Recovery failed: " + rollback.Message }; }
            return new[] { ex.Message };
        }
    }

    /// <summary>True for the --exit startup-task run: no window, no user, so no modal dialogs.</summary>
    private static bool _headless;
    public static string LogPath => Path.Combine(Store.RootDirectory, "roch-gpu.log");

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        Settings = Store.LoadSettings();
        Native.Theme.Apply(Settings.DarkMode);   // before any window exists, so none of them flashes the other mode

        // Claimed here rather than in the entry point because the elevated relaunch is a new process:
        // the unelevated one that started it has already gone, and whichever copy actually runs the
        // window is the one that should hold the handle.
        if (!Native.SingleInstance.Claim())
        {
            Native.SingleInstance.AskExistingToShow();
            Shutdown(0);
            return;
        }

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
                    "Roch GPU — no supported GPU", MessageBoxButton.YesNo, MessageBoxImage.Warning);
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

        // Recover BEFORE processing a logon profile so a crashed trial cannot be replayed at boot.
        bool recoveredTrial = false;
        try
        {
            var d = Service.Device;
            Recovery = new ProfileRecovery(Store.RootDirectory, $"{d.Vendor}|{d.BusId}|{d.Name}",
                TuningProfile.Stock(Service.Capabilities, d.Name), Service.Apply);
            if (Recovery.HasPending)
            {
                recoveredTrial = true;
                var errors = Recovery.Revert();
                RecoveryNotice = errors.Count == 0 || TuningService.OnlyNotes(errors)
                    ? "Recovered interrupted profile trial. Startup apply was skipped."
                    : "Profile recovery failed: " + string.Join("; ", errors);
                LogLine(RecoveryNotice);
            }
        }
        catch (Exception ex)
        {
            RecoveryNotice = "Profile recovery unavailable: " + ex.Message;
            LogLine(RecoveryNotice); recoveredTrial = true;
        }
        if (recoveredTrial) { applyProfile = null; minimized = false; }

        if (applyProfile != null)
        {
            TuningProfile? p = null;
            try { p = Recovery?.StartupProfile(applyProfile); }
            catch (Exception ex) { LogLine($"Profile '{applyProfile}' could not be read: {ex.Message}"); }
            if (p == null)
            {
                RecoveryNotice = $"Confirm '{applyProfile}' using the Startup checkbox before it can run at logon.";
                LogLine(RecoveryNotice); minimized = false;
            }
            else
            {
                try
                {
                    var errs = Recovery!.Begin(p);
                    if (errs.Count == 0 || TuningService.OnlyNotes(errs))
                    {
                        Recovery.Confirm();
                        LogLine($"Confirmed startup profile '{applyProfile}' applied.");
                    }
                    else
                    {
                        var rollback = Recovery.Revert();
                        RecoveryNotice = "Startup apply failed: " + string.Join("; ", errs)
                            + (Recovery.HasPending ? " Recovery failed: " + string.Join("; ", rollback) : " — restored fallback profile.");
                        LogLine(RecoveryNotice); minimized = false;
                    }
                }
                catch (Exception ex)
                {
                    RecoveryNotice = "Startup recovery interrupted: " + ex.Message;
                    LogLine(RecoveryNotice); minimized = false;
                }
            }
            if (exitAfter)
            {
                // Fan curve can't survive without a resident process; the CLI/startup docs say so.
                Service.Dispose();
                Shutdown(0);
                return;
            }
        }

        if (exitAfter) { Service.Dispose(); Shutdown(0); return; }
        var win = new MainWindow(Service, minimized);
        MainWindow = win;
        win.Show();
        if (minimized) win.MinimizeToTray();
        // After Show, so the monitor can position itself beside a window that has a size.
        if (Settings.AutoOpenMonitor) win.SetMonitorOpen(true);
    }

    private void OnUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        LogLine("Unhandled: " + e.Exception);
        // A message box in the logon-task run would wait forever on a desktop nobody is looking at.
        if (!_headless)
            MessageBox.Show(e.Exception.Message, "Roch GPU error", MessageBoxButton.OK, MessageBoxImage.Error);
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
