using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Linq;
using GpuTuner.Core.Models;

namespace GpuTuner.App.ViewModels;

/// <summary>
/// One line of the telemetry table: either a group heading or a sensor with its four figures.
/// </summary>
public sealed class TelemetryRow : ObservableObject
{
    public string Name { get; }
    public bool IsHeader { get; }
    /// <summary>The Parameter/Current/Min/Max/Average strip repeated under each section title.</summary>
    public bool IsColumnHeader { get; }
    /// <summary>Alternating band, recomputed as rows are shown and hidden.</summary>
    public bool IsBanded { get => _banded; set => Set(ref _banded, value); }
    private bool _banded;

    /// <summary>Heading this row folds under, or null when it always shows.</summary>
    public string? Group { get; }
    public bool IsVisible { get => _visible; set => Set(ref _visible, value); }
    private bool _visible = true;

    /// <summary>Headings carry the fold marker; sensors leave it empty.</summary>
    public string Marker { get => _marker; private set => Set(ref _marker, value); }
    private string _marker = "";

    public string Current { get => _current; private set => Set(ref _current, value); }
    public string Minimum { get => _min; private set => Set(ref _min, value); }
    public string Maximum { get => _max; private set => Set(ref _max, value); }
    public string Average { get => _avg; private set => Set(ref _avg, value); }
    private string _current = Dash, _min = Dash, _max = Dash, _avg = Dash;

    private const string Dash = "—";

    private readonly SensorStat? _stat;
    private readonly string _unit;
    private readonly int _decimals;

    private TelemetryRow(string name, bool isHeader, string? group, SensorStat? stat, string unit, int decimals, bool isColumnHeader = false)
    {
        Name = name; IsHeader = isHeader; Group = group; IsColumnHeader = isColumnHeader;
        _stat = stat; _unit = unit; _decimals = decimals;
        if (isColumnHeader) { _current = "Current"; _min = "Min"; _max = "Max"; _avg = "Average"; return; }
        if (!isHeader) return;
        Marker = "▾ ";
        // A heading has no reading of its own, and an em dash in all four columns reads as four
        // sensors that failed rather than as a title.
        _current = _min = _max = _avg = "";
    }

    public static TelemetryRow Header(string name) => new(name, true, null, null, "", 0);

    /// <summary>
    /// Repeated under every section rather than printed once at the top. A single strip scrolls away
    /// and leaves four unlabelled columns of numbers for the rest of the table.
    /// </summary>
    public static TelemetryRow Columns(string group) => new("Parameter", false, group, null, "", 0, isColumnHeader: true);

    public static TelemetryRow Sensor(string name, string group, SensorStat stat, string unit, int decimals = 0) =>
        new(name, false, group, stat, unit, decimals);

    public void SetExpanded(bool expanded) => Marker = expanded ? "▾ " : "▸ ";

    /// <summary>Re-read the statistic into the four display strings.</summary>
    public void Refresh()
    {
        if (_stat == null) return;
        Current = Format(_stat.Current);
        Minimum = Format(_stat.Minimum);
        Maximum = Format(_stat.Maximum);
        Average = Format(_stat.Average);
    }

    private string Format(double v) =>
        double.IsNaN(v) ? Dash : v.ToString("F" + _decimals, CultureInfo.CurrentCulture) + " " + _unit;
}

/// <summary>
/// The whole table: which sensors exist for this card, their running statistics, and the rows that
/// display them. Built once when the monitor opens, then fed a sample at a time.
///
/// Rows are held flat rather than nested so the banding can count only what is visible — banding by
/// position breaks the moment a group is folded away, leaving two shaded rows adjacent.
/// </summary>
public sealed class TelemetryTable
{
    public ObservableCollection<TelemetryRow> Rows { get; } = new();

    private readonly Dictionary<string, SensorStat> _stats = new();
    private readonly Dictionary<string, bool> _expanded = new();
    private readonly List<TelemetryRow> _all = new();

    private SensorStat Stat(string key)
    {
        if (!_stats.TryGetValue(key, out var s)) _stats[key] = s = new SensorStat();
        return s;
    }

    private string? _group;

    private void Group(string name)
    {
        _group = name;
        _expanded[name] = true;
        Add(TelemetryRow.Header(name.ToUpperInvariant()));
        Add(TelemetryRow.Columns(name));
    }

    private void Sensor(string key, string label, string unit, int decimals = 0) =>
        Add(TelemetryRow.Sensor(label, _group!, Stat(key), unit, decimals));

    private void Add(TelemetryRow r) { _all.Add(r); Rows.Add(r); }

    /// <summary>
    /// Lay out the sensors this card actually has. A row for a sensor the card never reports would
    /// sit at an em dash for the whole session, which reads as a fault rather than an absence.
    /// </summary>
    public TelemetryTable(GpuTelemetry first, GpuCapabilities caps)
    {
        Group("Temperatures");
        Sensor("temp", "GPU", "°C", 1);
        if (!double.IsNaN(first.HotSpotC)) Sensor("hotspot", "Hot spot", "°C", 1);
        if (!double.IsNaN(first.MemoryTemperatureC)) Sensor("memtemp", "Memory junction", "°C", 1);

        Group("Voltages");
        if (!double.IsNaN(first.VoltageMv)) Sensor("volt", "GPU core (NVVDD)", "mV");

        Group("Clocks");
        Sensor("core", "GPU core", "MHz");
        Sensor("mem", "Memory", "MHz");
        if (caps.CanSetXbarOffset) Sensor("xbar", "Crossbar", "MHz");
        if (caps.CanSetSysOffset) Sensor("sys", "SYS", "MHz");
        if (caps.CanSetVideoOffset) Sensor("video", "Video", "MHz");

        Group("Load");
        Sensor("load", "GPU core", "%");
        Sensor("memload", "Memory controller", "%");

        Group("Power");
        if (first.PowerWatts > 0) Sensor("watts", "Board", "W", 1);
        Sensor("tdp", "Total, % of TDP", "%", 1);

        Group("Fans");
        int fans = Math.Max(first.FanRpms.Length, 1);
        for (int i = 0; i < fans; i++)
        {
            Sensor($"fanrpm{i}", fans == 1 ? "Speed" : $"Fan {i + 1}", "RPM");
            Sensor($"fanpct{i}", fans == 1 ? "Duty" : $"Fan {i + 1} duty", "%");
        }

        Group("Memory");
        Sensor("memused", "Allocated", "MB");

        Restripe();
    }

    /// <summary>Section titles are shown in caps; this maps one back to its group key.</summary>
    public string? GroupForTitle(string title) =>
        _expanded.Keys.FirstOrDefault(k => k.ToUpperInvariant() == title);

    /// <summary>Fold a group open or shut.</summary>
    public void Toggle(string group)
    {
        bool expanded = !_expanded.GetValueOrDefault(group, true);
        _expanded[group] = expanded;
        foreach (var r in _all)
        {
            if (r.IsHeader && r.Name == group.ToUpperInvariant()) r.SetExpanded(expanded);
            else if (r.Group == group) r.IsVisible = expanded;
        }
        Restripe();
    }

    /// <summary>Band by visible position, so folding a group cannot leave two shaded rows adjacent.</summary>
    private void Restripe()
    {
        int visible = 0;
        foreach (var r in _all)
        {
            if (!r.IsVisible) continue;
            r.IsBanded = !r.IsHeader && !r.IsColumnHeader && visible % 2 == 1;
            visible++;
        }
    }

    /// <summary>Feed one sample in and refresh the display strings.</summary>
    public void Add(GpuTelemetry t, IReadOnlyDictionary<string, double>? extraClocks = null)
    {
        void Put(string key, double v) { if (_stats.TryGetValue(key, out var s)) s.Add(v); }

        Put("temp", t.TemperatureC);
        Put("hotspot", t.HotSpotC);
        Put("memtemp", t.MemoryTemperatureC);
        Put("volt", t.VoltageMv);
        Put("core", t.CoreClockMhz);
        Put("mem", t.MemoryClockMhz);
        Put("load", t.GpuLoadPercent);
        Put("memload", t.MemoryLoadPercent);
        Put("watts", t.PowerWatts);
        Put("tdp", t.PowerPercent);
        Put("memused", t.MemoryUsedMb);

        for (int i = 0; i < t.FanRpms.Length; i++) Put($"fanrpm{i}", t.FanRpms[i]);
        for (int i = 0; i < t.FanPercents.Length; i++) Put($"fanpct{i}", t.FanPercents[i]);
        if (t.FanRpms.Length == 0) { Put("fanrpm0", t.FanRpm); Put("fanpct0", t.FanPercent); }

        if (extraClocks != null)
            foreach (var (k, v) in extraClocks) Put(k, v);

        foreach (var r in _all) r.Refresh();
    }

    public void ResetStats()
    {
        foreach (var s in _stats.Values) s.Reset();
        foreach (var r in _all) r.Refresh();
    }
}
