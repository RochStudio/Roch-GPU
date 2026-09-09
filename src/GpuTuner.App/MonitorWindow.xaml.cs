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
/// The hardware monitor, in its own window. The fan curve moved to the fan window, where the
/// rest of the fan controls are - under a table of sensor readings was never where it belonged.
///
/// It owns its telemetry subscription rather than being fed by the main window, so opening and
/// closing it costs nothing but the subscription — and the main window stays a narrow strip of
/// controls whether this is open or not.
/// </summary>
public partial class MonitorWindow : Window
{
    private readonly TuningService _svc;
    private readonly MainViewModel _vm;

    public MonitorWindow(TuningService svc, MainViewModel vm)
    {
        _svc = svc;
        _vm = vm;
        DataContext = vm;
        InitializeComponent();
        AutoOpen.IsChecked = App.Settings.AutoOpenMonitor;

        // Draw the last sample immediately instead of waiting for the next poll.
        if (svc.Latest != null) Render(svc.Latest);
        svc.TelemetryUpdated += OnTelemetry;

        Theme.Register(this);   // paints the chrome now, and follows every later light/dark switch
        Closed += (_, _) =>
        {
            _svc.TelemetryUpdated -= OnTelemetry;
        };
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

    private static string Summary(double value, string unit, string format) =>
        double.IsFinite(value) ? value.ToString(format) + " " + unit : "—";

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
        TemperatureSummary.Text = Summary(t.TemperatureC, "°C", "0.0");
        ClockSummary.Text = Summary(t.CoreClockMhz, "MHz", "0");
        PowerSummary.Text = Summary(t.PowerWatts, "W", "0.0");
        int sensors = _table.Rows.Count(r => !r.IsHeader && !r.IsColumnHeader);
        Elapsed.Text = $"{sensors} sensors  ·  " + (DateTime.UtcNow - _statsSince).ToString(@"hh\:mm\:ss");

    }


}
