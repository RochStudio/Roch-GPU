using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Linq;
using GpuTuner.Core.Backends.Nvidia;
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


    public string Current { get => _current; private set => Set(ref _current, value); }
    public string Minimum { get => _min; private set => Set(ref _min, value); }
    public string Maximum { get => _max; private set => Set(ref _max, value); }
    public string Average { get => _avg; private set => Set(ref _avg, value); }
    private string _current = Dash, _min = Dash, _max = Dash, _avg = Dash;

    private const string Dash = "—";

    private readonly SensorStat? _stat;
    private readonly string _unit;
    private readonly int _decimals;

    private TelemetryRow(string name, bool isHeader, SensorStat? stat, string unit, int decimals, bool isColumnHeader = false)
    {
        Name = name; IsHeader = isHeader; IsColumnHeader = isColumnHeader;
        _stat = stat; _unit = unit; _decimals = decimals;
        if (isColumnHeader) { _current = "Current"; _min = "Min"; _max = "Max"; _avg = "Average"; return; }
        if (!isHeader) return;
        // A heading has no reading of its own, and an em dash in all four columns reads as four
        // sensors that failed rather than as a title.
        _current = _min = _max = _avg = "";
    }

    public static TelemetryRow Header(string name) => new(name, true, null, "", 0);

    /// <summary>
    /// Repeated under every section rather than printed once at the top. A single strip scrolls away
    /// and leaves four unlabelled columns of numbers for the rest of the table.
    /// </summary>
    public static TelemetryRow Columns() => new("Parameter", false, null, "", 0, isColumnHeader: true);

    public static TelemetryRow Sensor(string name, SensorStat stat, string unit, int decimals = 0) =>
        new(name, false, stat, unit, decimals);

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
    private readonly List<TelemetryRow> _all = new();
    private readonly HashSet<string> _supplementalKeys = new();

    private SensorStat Stat(string key)
    {
        if (!_stats.TryGetValue(key, out var s)) _stats[key] = s = new SensorStat();
        return s;
    }

    private void Group(string name)
    {
        Add(TelemetryRow.Header(name.ToUpperInvariant()));
    }

    private void Sensor(string key, string label, string unit, int decimals = 0) =>
        Add(TelemetryRow.Sensor(label, Stat(key), unit, decimals));

    private void Add(TelemetryRow r) { _all.Add(r); Rows.Add(r); }

    /// <summary>
    /// Lay out the sensors this card actually has. A row for a sensor the card never reports would
    /// sit at an em dash for the whole session, which reads as a fault rather than an absence.
    /// </summary>
    public TelemetryTable(GpuTelemetry first, GpuCapabilities caps,
                          IEnumerable<string>? extraClockKeys = null)
    {
        extraClockKeys ??= Array.Empty<string>();

        Group("Temperatures");
        Sensor("temp", "GPU", "°C", 1);
        if (!double.IsNaN(first.HotSpotC)) Sensor("hotspot", "Hot spot", "°C", 1);
        if (!double.IsNaN(first.MemoryTemperatureC)) Sensor("memtemp", "Memory junction", "°C", 1);

        Group("Voltages");
        if (!double.IsNaN(first.VoltageMv)) Sensor("volt", "GPU core (VID)", "mV");
        if (!double.IsNaN(first.NvvddMv)) Sensor("nvvdd", "NVVDD (measured)", "mV", 1);
        if (!double.IsNaN(first.MsvddMv)) Sensor("msvdd", "MSVDD (measured)", "mV", 1);

        Group("Clocks");
        Sensor("core", "GPU core", "MHz");
        if (!caps.PowerLimitIsOffset) Sensor("coremeasured", "GPU core (effective)", "MHz");
        Sensor("mem", "Memory", "MHz");
        if (!double.IsNaN(first.FabricClockMhz)) Sensor("fclk", "Fabric (FCLK)", "MHz");
        if (!double.IsNaN(first.SocClockMhz)) Sensor("socclk", "SoC", "MHz");
        if (caps.CanSetXbarOffset) Sensor("xbar", "Crossbar", "MHz");
        if (caps.CanSetSysOffset) Sensor("sys", "SYS", "MHz");
        if (caps.CanSetVideoOffset) Sensor("video", "Video", "MHz");
        // Whatever else this card reports. The backend supplies the name where one has been checked
        // against the hardware and the bare type number where it has not. Built from the first
        // sample, so a card that reports nothing extra gets no rows rather than a column of dashes.
        foreach (var key in extraClockKeys)
            if (key.StartsWith("domain", StringComparison.Ordinal)
                && int.TryParse(key["domain".Length..], out int type))
                Sensor(key, NvApiBackend.DomainName(type), "MHz");

        Group("Load");
        Sensor("load", "GPU core", "%");
        Sensor("memload", "Memory controller", "%");

        Group("Power");
        if (first.PowerWatts > 0) Sensor("watts", "Board draw", "W", 1);
        if (!caps.PowerLimitIsOffset || double.IsFinite(first.PowerPercent))
            Sensor("tdp", "Total, % of TDP", "%", 1);

        // Measured 12 V rail currents, straight from the card's power monitor. Index 0 is the board
        // total; the rest are the supply rails behind it, named by their voltage since the driver
        // gives no names. These are NOT the current an OCP limit guards — those rails sit after the
        // VRM at about a volt, and this family does not report them.
        if (first.RailAmps.Length > 0)
        {
            Group("Rail current");
            for (int i = 0; i < first.RailAmps.Length; i++)
                Sensor($"railA{i}", NvApiBackend.PowerRailName(i, first.RailAmps.Length), "A", 2);
        }

        Group("Fans");
        int fans = Math.Max(first.FanRpms.Length, 1);
        for (int i = 0; i < fans; i++)
        {
            Sensor($"fanrpm{i}", fans == 1 ? "Speed" : $"Fan {i + 1}", "RPM");
            Sensor($"fanpct{i}", fans == 1 ? "Duty" : $"Fan {i + 1} duty", "%");
        }

        Group("Memory");
        Sensor("memused", caps.PowerLimitIsOffset ? "Dedicated VRAM used" : "Allocated", "MB");

        Restripe();
    }

    /// <summary>Alternating bands on the data rows; titles and column strips carry their own tone.</summary>
    private void Restripe()
    {
        int data = 0;
        foreach (var r in _all)
        {
            if (r.IsHeader || r.IsColumnHeader) { data = 0; continue; }
            r.IsBanded = data % 2 == 1;
            data++;
        }
    }

    /// <summary>Feed one sample in and refresh the display strings.</summary>
    public void Add(GpuTelemetry t, IReadOnlyDictionary<string, double>? extraClocks = null)
    {
        void Put(string key, double v) { if (_stats.TryGetValue(key, out var s)) s.Add(v); }

        // Some sensors become available after the first poll (driver warmup or waking from idle).
        void Discover(string key, string name, string unit, double value)
        {
            if (double.IsFinite(value) && !_stats.ContainsKey(key)) InsertSensor(key, name, unit);
        }
        Discover("hotspot", "Hot spot", "°C", t.HotSpotC);
        Discover("memtemp", "Memory junction", "°C", t.MemoryTemperatureC);
        Discover("volt", "GPU core (VID)", "mV", t.VoltageMv);
        Discover("watts", "Board draw", "W", t.PowerWatts);
        Discover("fclk", "Fabric (FCLK)", "MHz", t.FabricClockMhz);
        Discover("socclk", "SoC", "MHz", t.SocClockMhz);

        Put("temp", t.TemperatureC);
        Put("hotspot", t.HotSpotC);
        Put("memtemp", t.MemoryTemperatureC);
        Put("volt", t.VoltageMv);
        Put("nvvdd", t.NvvddMv);
        Put("msvdd", t.MsvddMv);
        Put("core", t.CoreClockMhz);
        Put("mem", t.MemoryClockMhz);
        Put("fclk", t.FabricClockMhz);
        Put("socclk", t.SocClockMhz);
        Put("load", t.GpuLoadPercent);
        Put("memload", t.MemoryLoadPercent);
        Put("watts", t.PowerWatts);
        for (int i = 0; i < t.RailAmps.Length; i++) Put($"railA{i}", t.RailAmps[i]);
        Put("tdp", t.PowerPercent);
        Put("memused", t.MemoryUsedMb);
        bool newRows = false;
        foreach (var sensor in t.SupplementalSensors)
        {
            if (_supplementalKeys.Add(sensor.Key))
            {
                InsertSensor(sensor.Key, sensor.Name, sensor.Unit);
                newRows = true;
            }
        }
        var present = t.SupplementalSensors.Select(s => s.Key).ToHashSet();
        foreach (var key in _supplementalKeys)
            if (!present.Contains(key)) Put(key, double.NaN);
        foreach (var sensor in t.SupplementalSensors) Put(sensor.Key, sensor.Value);
        if (newRows) Restripe();

        for (int i = 0; i < t.FanRpms.Length; i++) Put($"fanrpm{i}", t.FanRpms[i]);
        for (int i = 0; i < t.FanPercents.Length; i++) Put($"fanpct{i}", t.FanPercents[i]);
        if (t.FanRpms.Length == 0) { Put("fanrpm0", t.FanRpm); Put("fanpct0", t.FanPercent); }

        if (extraClocks != null)
            foreach (var (k, v) in extraClocks) Put(k, v);

        foreach (var r in _all) r.Refresh();
    }

    private void InsertSensor(string key, string name, string unit)
    {
        string group = unit switch
        {
            "°C" => "TEMPERATURES", "mV" or "V" => "VOLTAGES", "MHz" => "CLOCKS",
            "W" => "POWER", "MB" => "MEMORY", "gen" or "lanes" => "PCIe LINK",
            _ => "LIMITERS"
        };
        int heading = _all.FindIndex(r => r.IsHeader && r.Name == group.ToUpperInvariant());
        if (heading < 0) { Group(group); heading = _all.Count - 1; }
        int end = _all.FindIndex(heading + 1, r => r.IsHeader);
        if (end < 0) end = _all.Count;
        int decimals = unit is "°C" or "W" ? 1 : 0;
        var row = TelemetryRow.Sensor(name, Stat(key), unit, decimals);
        _all.Insert(end, row);
        Rows.Insert(end, row);
        Restripe();
    }

    public void ResetStats()
    {
        foreach (var s in _stats.Values) s.Reset();
        foreach (var r in _all) r.Refresh();
    }
}
