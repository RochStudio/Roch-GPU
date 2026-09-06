using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Linq;
using System.Windows;
using System.Windows.Input;
using GpuTuner.App.Native;
using GpuTuner.App.ViewModels;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;
using WinForms = System.Windows.Forms;

namespace GpuTuner.App;

public partial class MainWindow : Window
{
    private readonly TuningService _svc;
    private readonly MainViewModel _vm;
    private WinForms.NotifyIcon? _tray;
    private System.Drawing.Icon? _trayIcon;
    private bool _reallyClose;

    /// <summary>
    /// The app icon at whatever size the shell wants for the notification area. The .ico carries a
    /// hand-tuned 16px frame — no grid, no selected point, just the curve — so it is worth asking for
    /// the small size and letting the icon pick that frame, rather than letting NotifyIcon downscale
    /// the 256px one into mush. Returns null if the resource is missing, so the caller can fall back.
    /// </summary>
    private static System.Drawing.Icon? LoadEmbeddedIcon()
    {
        try
        {
            using var s = System.Reflection.Assembly.GetExecutingAssembly()
                                .GetManifestResourceStream("RochGPU.ico");
            return s == null ? null : new System.Drawing.Icon(s, WinForms.SystemInformation.SmallIconSize);
        }
        catch { return null; }
    }

    public MainWindow(TuningService svc, bool startMinimized)
    {
        _svc = svc;
        _vm = new MainViewModel(svc, App.Store);
        DataContext = _vm;                // set BEFORE InitializeComponent so Slider Min/Max bind before Value
        InitializeComponent();

        svc.TelemetryUpdated += OnTelemetry;
        svc.StartPolling(Math.Max(250, App.Settings.PollIntervalMs));

        Theme.Register(this);   // paints the chrome now, and follows every later light/dark switch
        // A second launch cannot open a window of its own, so it asks this one to come forward.
        Native.SingleInstance.ListenForShowRequests(() => Dispatcher.BeginInvoke(RestoreFromTray));
        ThemeButton.Content = Theme.IsDark ? "Dark" : "Light";
        SetupTray();
        UpdatePollDetail();              // monitor starts closed, so the poll starts paused
        Closing += MainWindow_Closing;
        StateChanged += (_, _) => { if (WindowState == WindowState.Minimized) MinimizeToTray(); };

        // Enter in any value box commits it (TextBoxes update on LostFocus, so push focus off the box).
        AddHandler(System.Windows.Controls.TextBox.KeyDownEvent, new System.Windows.Input.KeyEventHandler((s, e) =>
        {
            if (e.Key == System.Windows.Input.Key.Enter && s is System.Windows.Controls.TextBox tb)
            {
                var expr = tb.GetBindingExpression(System.Windows.Controls.TextBox.TextProperty);
                expr?.UpdateSource();
                e.Handled = true;
            }
        }));
    }

    // ------------------------------------------------------------------ telemetry → UI
    private void OnTelemetry(GpuTelemetry t)
    {
        // Called on the polling thread. The graphs live in MonitorWindow, which subscribes itself.
        Dispatcher.BeginInvoke(() =>
        {
            // A sample can already be queued when polling pauses — the first poll fires before the
            // constructor has finished wiring this up. Dropping it here keeps a stale reading off
            // the limiter line, which is the one thing on this window fed by telemetry.
            if (_svc.BackgroundMode) return;
            _vm.Telemetry = t;
            if (_tray != null) _tray.Text = $"Roch GPU — {t.TemperatureC:0}°C, {t.CoreClockMhz:0} MHz, fan {t.FanPercent:0}%";
        });
    }

    private void CurveEditor_Click(object sender, RoutedEventArgs e)
    {
        // Non-modal so the graphs keep updating behind it; one instance at a time.
        if (_curveWindow != null)
        {
            if (_curveWindow.WindowState == WindowState.Minimized) _curveWindow.WindowState = WindowState.Normal;
            _curveWindow.Activate();
            return;
        }
        _curveWindow = new CurveWindow(_svc) { Owner = this };
        _curveWindow.Closed += (_, _) => _curveWindow = null;
        _curveWindow.Show();
    }
    private CurveWindow? _curveWindow;

    private void Xoc_Click(object sender, RoutedEventArgs e)
    {
        // Same one-instance, non-modal handling as the curve editor: the graphs keep running behind it.
        if (_xocWindow != null)
        {
            if (_xocWindow.WindowState == WindowState.Minimized) _xocWindow.WindowState = WindowState.Normal;
            _xocWindow.Activate();
            return;
        }
        _xocWindow = new XocWindow(_vm) { Owner = this };
        _xocWindow.Closed += (_, _) => _xocWindow = null;
        _xocWindow.Show();
    }
    private XocWindow? _xocWindow;

    // ------------------------------------------------------------------ custom title bar
    //
    // WindowStyle=None means we own the caption. Windows will only ever grey a maximize button out,
    // never remove it, so the only way to be rid of it is to draw the bar ourselves.

    private void TitleBar_MouseDown(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton != MouseButton.Left) return;
        // DragMove throws if the button is no longer down by the time it runs (fast click, or the
        // press was consumed elsewhere), and an unhandled exception here would kill the app.
        try { DragMove(); } catch (InvalidOperationException) { }
    }

    private void TitleMinimize_Click(object sender, RoutedEventArgs e) => WindowState = WindowState.Minimized;

    /// <summary>Goes through Close() so the "fan control is active" prompt still runs.</summary>
    /// <summary>
    /// The fan window. Owned like the others: one at a time, and it goes away with this window.
    /// </summary>
    private FanWindow? _fanWindow;

    private void Fan_Click(object sender, RoutedEventArgs e)
    {
        if (_fanWindow is { IsLoaded: true }) { _fanWindow.Activate(); return; }
        _fanWindow = new FanWindow(_svc, _vm) { Owner = this };
        _fanWindow.Closed += (_, _) => { _fanWindow = null; UpdatePollDetail(); };
        _fanWindow.Show();
        UpdatePollDetail();   // it wants live readings, so the poll has to stop being a background one
    }

    private void Theme_Click(object sender, RoutedEventArgs e)
    {
        bool dark = Theme.Toggle();
        ThemeButton.Content = dark ? "Dark" : "Light";
        App.Settings.DarkMode = dark;
        App.Store.SaveSettings(App.Settings);
    }

    private void TitleClose_Click(object sender, RoutedEventArgs e) => Close();

    /// <summary>
    /// The footer link. UseShellExecute is what sends a URL to the browser rather than trying to run
    /// it as a program; without it this throws. A browser that will not open is worth a status line,
    /// not a crash dialog on top of a tuning tool.
    /// </summary>
    private void XLink_Click(object sender, MouseButtonEventArgs e)
    {
        const string url = "https://x.com/MateoPCTech";
        try { Process.Start(new ProcessStartInfo(url) { UseShellExecute = true }); }
        catch (Exception ex) { _vm.Status = $"Could not open {url}: {ex.Message}"; _vm.StatusIsError = true; }
    }

    /// <summary>Right-click a numbered profile slot to wipe it. Left-click loads/saves (see MainViewModel).</summary>
    private void Slot_RightClick(object sender, MouseButtonEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: ProfileSlot slot })
        {
            _vm.ClearSlot(slot.Number);
            e.Handled = true;
        }
    }

    private void OpenLog_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            if (!System.IO.File.Exists(App.LogPath)) System.IO.File.WriteAllText(App.LogPath, "");
            Process.Start(new ProcessStartInfo(App.LogPath) { UseShellExecute = true });
        }
        catch (Exception ex) { MessageBox.Show(ex.Message); }
    }

    // ------------------------------------------------------------------ tray
    private void SetupTray()
    {
        try
        {
            _trayIcon = LoadEmbeddedIcon();
            _tray = new WinForms.NotifyIcon
            {
                // Falls back to the generic Windows icon rather than failing the tray entirely, which
                // is what SetupTray's catch would otherwise do to the Show/Exit menu.
                Icon = _trayIcon ?? System.Drawing.SystemIcons.Application,
                Text = "Roch GPU",
                Visible = true
            };
            var menu = new WinForms.ContextMenuStrip();
            menu.Items.Add("Show", null, (_, _) => RestoreFromTray());
            menu.Items.Add("Reset GPU to defaults", null, (_, _) => _vm.ResetCommand.Execute(null));
            menu.Items.Add(new WinForms.ToolStripSeparator());
            menu.Items.Add("Exit", null, (_, _) => { _reallyClose = true; Close(); });
            _tray.ContextMenuStrip = menu;
            _tray.DoubleClick += (_, _) => RestoreFromTray();
        }
        catch { _tray = null; }
    }

    /// <summary>
    /// The hardware monitor is a separate window, opened on demand — it never opens by itself with
    /// the app. Clicking Monitor opens it, or brings it forward if it is already up; clicking it
    /// again while it has focus closes it.
    /// </summary>
    private void ToggleMonitor_Click(object sender, RoutedEventArgs e)
    {
        if (_monitorWindow == null) { SetMonitorOpen(true); return; }
        if (_monitorWindow.IsActive) SetMonitorOpen(false);
        else _monitorWindow.Activate();
    }

    private MonitorWindow? _monitorWindow;

    public void SetMonitorOpen(bool open)
    {
        if (open)
        {
            if (_monitorWindow != null) { _monitorWindow.Activate(); return; }
            _monitorWindow = new MonitorWindow(_svc, _vm)
            {
                Owner = this,
                Left = Left + Width + 8,
                Top = Top
            };
            _monitorWindow.Closed += (_, _) => { _monitorWindow = null; UpdatePollDetail(); };
            _monitorWindow.Show();
        }
        else
        {
            _monitorWindow?.Close();      // the Closed handler clears the field
            _monitorWindow = null;
        }
        UpdatePollDetail();
    }

    /// <summary>
    /// Reached from the title bar's minimise (WindowState change) and from the "keep the fan curve
    /// running" prompt on close. There is no button for it: minimising is the gesture.
    /// </summary>
    public void MinimizeToTray()
    {
        if (_tray == null) { WindowState = WindowState.Minimized; return; }
        Hide();
        ShowInTaskbar = false;
    }

    internal void RestoreFromTray()
    {
        Show();
        ShowInTaskbar = true;
        WindowState = WindowState.Normal;
        Activate();
    }

    /// <summary>
    /// The hardware monitor is the only thing that displays telemetry, so it is the only thing that
    /// justifies sampling it. With it closed the poll makes no driver call at all — a full sample is
    /// ~13 ms of synchronous NVAPI work, and paying that behind a game buys nothing when there is no
    /// graph to draw it on. A running fan curve still gets its temperature, at 0.02 ms.
    ///
    /// Tray state is deliberately not part of this condition: the main window shows no live telemetry
    /// beyond the limiter line, which is cleared below rather than left showing a stale reading.
    /// </summary>
    private void UpdatePollDetail()
    {
        // Any window showing live readings counts, not just the monitor. The fan window shows a duty
        // and an RPM per fan, and background polling raises no telemetry at all - so leaving it out
        // of this left three readouts sitting at an em dash for as long as the window was open,
        // which reads as three broken sensors rather than as a poll nobody asked for.
        bool paused = _monitorWindow == null && _fanWindow == null;
        _svc.BackgroundMode = paused;
        if (!paused) return;

        // Nothing will arrive to refresh these now, so don't leave the last sample on screen
        // pretending to still be current.
        _vm.Telemetry = null;
        if (_tray != null) _tray.Text = "Roch GPU";
    }

    private void MainWindow_Closing(object? sender, CancelEventArgs e)
    {
        // Closing the window while a curve/fixed fan is active would silently hand fans back to auto
        // (App.OnExit does that). Ask, so nobody loses their fan curve by accident.
        //
        // Only where this app is the thing enforcing it. A driver-owned curve survives exit on its
        // own, so the prompt would be offering to protect something that was never at risk.
        if (!_reallyClose && _tray != null && !_svc.Capabilities.FanCurveIsHardware
            && (_vm.IsCurveFan || _vm.IsFixedFan))
        {
            var r = MessageBox.Show(
                "Fan control is active. Minimize to tray to keep it running?\n\nYes = minimize to tray\nNo = exit (fans return to automatic)",
                "Roch GPU", MessageBoxButton.YesNoCancel, MessageBoxImage.Question);
            if (r == MessageBoxResult.Cancel) { e.Cancel = true; return; }
            if (r == MessageBoxResult.Yes) { e.Cancel = true; MinimizeToTray(); return; }
        }
        _svc.TelemetryUpdated -= OnTelemetry;
        if (_monitorWindow != null) { _monitorWindow.Owner = null; _monitorWindow.Close(); _monitorWindow = null; }
        if (_tray != null) { _tray.Visible = false; _tray.Dispose(); _tray = null; }
        _trayIcon?.Dispose(); _trayIcon = null;   // owned here; SystemIcons fallbacks are shared and must not be
        Application.Current.Shutdown();
    }
}
