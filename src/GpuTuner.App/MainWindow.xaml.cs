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
    private bool _reallyClose;

    public MainWindow(TuningService svc, bool startMinimized)
    {
        _svc = svc;
        _vm = new MainViewModel(svc, App.Store);
        DataContext = _vm;                // set BEFORE InitializeComponent so Slider Min/Max bind before Value
        InitializeComponent();

        svc.TelemetryUpdated += OnTelemetry;
        svc.StartPolling(Math.Max(250, App.Settings.PollIntervalMs));

        WindowTheme.ApplyOnOpen(this);   // Windows draws the title bar, and it follows the OS theme, not ours
        SetupTray();
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
            _vm.Telemetry = t;
            if (_tray != null) _tray.Text = $"Roch GPU OC — {t.TemperatureC:0}°C, {t.CoreClockMhz:0} MHz, fan {t.FanPercent:0}%";
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
    private void TitleClose_Click(object sender, RoutedEventArgs e) => Close();

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
            _tray = new WinForms.NotifyIcon
            {
                Icon = System.Drawing.SystemIcons.Application,
                Text = "Roch GPU OC",
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
            _monitorWindow.Closed += (_, _) => _monitorWindow = null;
            _monitorWindow.Show();
        }
        else
        {
            _monitorWindow?.Close();      // the Closed handler clears the field
            _monitorWindow = null;
        }
    }

    private void Tray_Click(object sender, RoutedEventArgs e) => MinimizeToTray();

    public void MinimizeToTray()
    {
        if (_tray == null) { WindowState = WindowState.Minimized; return; }
        Hide();
        ShowInTaskbar = false;
    }

    private void RestoreFromTray()
    {
        Show();
        ShowInTaskbar = true;
        WindowState = WindowState.Normal;
        Activate();
    }

    private void MainWindow_Closing(object? sender, CancelEventArgs e)
    {
        // Closing the window while a curve/fixed fan is active would silently hand fans back to auto
        // (App.OnExit does that). Ask, so nobody loses their fan curve by accident.
        if (!_reallyClose && _tray != null && (_vm.IsCurveFan || _vm.IsFixedFan))
        {
            var r = MessageBox.Show(
                "Fan control is active. Minimize to tray to keep it running?\n\nYes = minimize to tray\nNo = exit (fans return to automatic)",
                "Roch GPU OC", MessageBoxButton.YesNoCancel, MessageBoxImage.Question);
            if (r == MessageBoxResult.Cancel) { e.Cancel = true; return; }
            if (r == MessageBoxResult.Yes) { e.Cancel = true; MinimizeToTray(); return; }
        }
        _svc.TelemetryUpdated -= OnTelemetry;
        if (_monitorWindow != null) { _monitorWindow.Owner = null; _monitorWindow.Close(); _monitorWindow = null; }
        if (_tray != null) { _tray.Visible = false; _tray.Dispose(); _tray = null; }
        Application.Current.Shutdown();
    }
}
