using System;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using GpuTuner.App.Native;
using GpuTuner.App.ViewModels;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

namespace GpuTuner.App;

/// <summary>
/// The hardware monitor and fan-curve editor, in their own window.
///
/// It owns its telemetry subscription rather than being fed by the main window, so opening and
/// closing it costs nothing but the subscription — and the main window stays a narrow strip of
/// controls whether this is open or not.
/// </summary>
public partial class MonitorWindow : Window
{
    private readonly TuningService _svc;
    private readonly MainViewModel _vm;
    private bool _suppressCurveEvents;

    public MonitorWindow(TuningService svc, MainViewModel vm)
    {
        _svc = svc;
        _vm = vm;
        DataContext = vm;
        _suppressCurveEvents = true;      // TextChanged fires while the XAML loads
        InitializeComponent();
        AutoOpen.IsChecked = App.Settings.AutoOpenMonitor;

        CurveEditor.SetPoints(_vm.EditorCurve.Points);
        HystBox.Text = _vm.EditorCurve.HysteresisC.ToString("0.#");
        StepBox.Text = _vm.EditorCurve.MinimumStepPercent.ToString("0.#");
        _suppressCurveEvents = false;

        CurveEditor.CurveChanged += (_, _) =>
        {
            if (_suppressCurveEvents) return;
            _vm.EditorCurve.Points = CurveEditor.Points.ToList();
            _vm.MarkCurveDirty();
        };
        _vm.PropertyChanged += Vm_PropertyChanged;

        // Draw the last sample immediately instead of waiting for the next poll.
        if (svc.Latest != null) Render(svc.Latest);
        svc.TelemetryUpdated += OnTelemetry;

        Theme.Register(this);   // paints the chrome now, and follows every later light/dark switch
        Closed += (_, _) =>
        {
            _svc.TelemetryUpdated -= OnTelemetry;
            _vm.PropertyChanged -= Vm_PropertyChanged;
        };
    }

    private void Vm_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(MainViewModel.EditorCurve)) return;
        _suppressCurveEvents = true;
        CurveEditor.SetPoints(_vm.EditorCurve.Points);
        HystBox.Text = _vm.EditorCurve.HysteresisC.ToString("0.#");
        StepBox.Text = _vm.EditorCurve.MinimumStepPercent.ToString("0.#");
        _suppressCurveEvents = false;
    }

    private void OnTelemetry(GpuTelemetry t) => Dispatcher.BeginInvoke(() => Render(t));

    /// <summary>
    /// The sensor table, built from the first sample because that is when the card has said which
    /// sensors it actually reports. A row for one it never reports would sit at an em dash all
    /// session, which reads as a fault rather than an absence.
    /// </summary>
    private TelemetryTable? _table;

    /// <summary>When the running figures were last started from nothing.</summary>
    private DateTime _statsSince = DateTime.UtcNow;

    private void AutoOpen_Click(object sender, RoutedEventArgs e)
    {
        App.Settings.AutoOpenMonitor = AutoOpen.IsChecked == true;
        App.Store.SaveSettings(App.Settings);
    }

    private void ResetStats_Click(object sender, RoutedEventArgs e)
    {
        _table?.ResetStats();
        _statsSince = DateTime.UtcNow;
    }

    private void Render(GpuTelemetry t)
    {
        var extra = _svc.MeasureExtraClocks();
        if (_table == null)
        {
            // The first sample says which domains this card actually reports, which is what decides
            // the rows - the same reason the sensor rows are built from it rather than from a list.
            _table = new TelemetryTable(t, _svc.Capabilities, extra.Keys);
            TableRows.ItemsSource = _table.Rows;
        }
        // The table is the only view, so these are always on screen and always worth reading.
        _table.Add(t, extra);
        Elapsed.Text = "Running " + (DateTime.UtcNow - _statsSince).ToString(@"hh\:mm\:ss");

        CurveEditor.SetLive(t.TemperatureC, t.FanPercent);
    }

    private void CurveParam_Changed(object sender, System.Windows.Controls.TextChangedEventArgs e)
    {
        if (_suppressCurveEvents) return;
        if (double.TryParse(HystBox.Text, out var h)) _vm.EditorCurve.HysteresisC = Math.Clamp(h, 0, 20);
        if (double.TryParse(StepBox.Text, out var s)) _vm.EditorCurve.MinimumStepPercent = Math.Clamp(s, 0, 20);
        _vm.MarkCurveDirty();
    }

    private void DefaultCurve_Click(object sender, RoutedEventArgs e)
    {
        _vm.EditorCurve.Points = FanCurve.DefaultPoints();
        _suppressCurveEvents = true;
        CurveEditor.SetPoints(_vm.EditorCurve.Points);
        _suppressCurveEvents = false;
        _vm.MarkCurveDirty();
    }
}
