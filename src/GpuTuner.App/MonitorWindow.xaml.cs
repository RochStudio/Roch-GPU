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
    private bool _powerInWatts;
    private double _peakCore;

    public MonitorWindow(TuningService svc, MainViewModel vm)
    {
        _svc = svc;
        _vm = vm;
        DataContext = vm;
        _suppressCurveEvents = true;      // TextChanged fires while the XAML loads
        InitializeComponent();

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

        int cap = svc.History.Capacity;
        foreach (var g in new[] { GCore, GVolt, GPower, GTemp, GLoad, GMem, GFan }) g.Capacity = cap;

        // Draw the backlog immediately instead of waiting for the next poll.
        if (svc.Latest != null) Render(svc.Latest);
        svc.TelemetryUpdated += OnTelemetry;

        WindowTheme.ApplyOnOpen(this);
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

    /// <summary>Only worth reading while the table is on screen; the graphs do not show them.</summary>
    private System.Collections.Generic.IReadOnlyDictionary<string, double>? ReadDomainClocks() =>
        ViewTable?.IsChecked == true ? _svc.MeasureExtraClocks() : null;

    private void View_Changed(object sender, RoutedEventArgs e)
    {
        if (GraphScroll == null || TableScroll == null) return;   // fires during InitializeComponent
        bool table = ViewTable.IsChecked == true;
        TableScroll.Visibility = table ? Visibility.Visible : Visibility.Collapsed;
        TableFooter.Visibility = table ? Visibility.Visible : Visibility.Collapsed;
        GraphScroll.Visibility = table ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>When the running figures were last started from nothing.</summary>
    private DateTime _statsSince = DateTime.UtcNow;

    private void ResetStats_Click(object sender, RoutedEventArgs e)
    {
        _table?.ResetStats();
        _statsSince = DateTime.UtcNow;
    }

    private void Render(GpuTelemetry t)
    {
        if (_table == null)
        {
            _table = new TelemetryTable(t, _svc.Capabilities);
            TableRows.ItemsSource = _table.Rows;
        }
        _table.Add(t, ReadDomainClocks());
        Elapsed.Text = "Running " + (DateTime.UtcNow - _statsSince).ToString(@"hh\:mm\:ss");

        VCore.Text = t.CoreClockMhz.ToString("0");
        if (t.CoreClockMhz > _peakCore)
        {
            _peakCore = t.CoreClockMhz;
            TCore.Text = $"Core clock, MHz   ·   peak {_peakCore:0}";
        }
        VVolt.Text = double.IsNaN(t.VoltageMv) ? "—" : t.VoltageMv.ToString("0");

        // AMD's PMLog reports board watts, not % of TDP. Switch the graph over the first time a
        // watt reading arrives, rather than drawing a permanently flat zero line.
        if (!_powerInWatts && t.PowerPercent <= 0 && t.PowerWatts > 0)
        {
            _powerInWatts = true;
            TPower.Text = "Power, W";
            GPower.AutoScale = true;
            GPower.MinSpan = 50;
            GPower.Unit = "W";
        }
        VPower.Text = (_powerInWatts ? t.PowerWatts : t.PowerPercent).ToString("0");

        VTemp.Text = t.TemperatureC.ToString("0");
        string tt = "GPU temperature, °C";
        if (!double.IsNaN(t.HotSpotC)) tt += $"   ·   hot spot {t.HotSpotC:0}°";
        if (!double.IsNaN(t.MemoryTemperatureC)) tt += $"   ·   memory {t.MemoryTemperatureC:0}°";
        TTemp.Text = tt;

        VLoad.Text = t.GpuLoadPercent.ToString("0");
        VMem.Text = t.MemoryClockMhz.ToString("0");
        VFan.Text = t.FanPercent.ToString("0");
        TFan.Text = t.FanRpm > 0 ? $"Fan speed, %   ·   {t.FanRpm:0} rpm" : "Fan speed, %";

        var hist = _svc.History.Snapshot();
        GCore.Values = hist.Select(h => h.CoreClockMhz).ToArray();
        GVolt.Values = hist.Select(h => h.VoltageMv).ToArray();
        GPower.Values = hist.Select(h => _powerInWatts ? h.PowerWatts : h.PowerPercent).ToArray();
        GTemp.Values = hist.Select(h => h.TemperatureC).ToArray();
        GLoad.Values = hist.Select(h => h.GpuLoadPercent).ToArray();
        GMem.Values = hist.Select(h => h.MemoryClockMhz).ToArray();
        GFan.Values = hist.Select(h => h.FanPercent).ToArray();

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
