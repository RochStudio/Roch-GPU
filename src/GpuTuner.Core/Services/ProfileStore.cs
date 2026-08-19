using System.Text.Json;
using System.Text.Json.Serialization;
using GpuTuner.Core.Models;

namespace GpuTuner.Core.Services;

/// <summary>Persists profiles + app settings as JSON under %AppData%\GpuTuner (or a custom folder).</summary>
public sealed class ProfileStore
{
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        WriteIndented = true,
        Converters = { new JsonStringEnumConverter() },
        DefaultIgnoreCondition = JsonIgnoreCondition.Never
    };

    public string RootDirectory { get; }
    public string ProfilesDirectory => Path.Combine(RootDirectory, "profiles");
    public string SettingsPath => Path.Combine(RootDirectory, "settings.json");

    public ProfileStore(string? rootDirectory = null)
    {
        RootDirectory = rootDirectory ??
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "RochGpuOC");
        Directory.CreateDirectory(ProfilesDirectory);
    }

    public IReadOnlyList<string> ListProfileNames() =>
        Directory.EnumerateFiles(ProfilesDirectory, "*.json")
            .Select(Path.GetFileNameWithoutExtension)
            .Where(n => n != null)
            .Select(n => n!)
            .OrderBy(n => n, StringComparer.OrdinalIgnoreCase)
            .ToList();

    public TuningProfile? Load(string name)
    {
        var path = PathFor(name);
        if (!File.Exists(path)) return null;
        var json = File.ReadAllText(path);
        var p = JsonSerializer.Deserialize<TuningProfile>(json, JsonOpts);
        if (p != null) p.Name = name;
        return p;
    }

    public void Save(TuningProfile profile)
    {
        profile.ModifiedUtc = DateTime.UtcNow;
        var json = JsonSerializer.Serialize(profile, JsonOpts);
        var tmp = PathFor(profile.Name) + ".tmp";
        File.WriteAllText(tmp, json);
        File.Move(tmp, PathFor(profile.Name), overwrite: true);
    }

    public bool Delete(string name)
    {
        var path = PathFor(name);
        if (!File.Exists(path)) return false;
        File.Delete(path);
        return true;
    }

    public AppSettings LoadSettings()
    {
        if (!File.Exists(SettingsPath)) return new AppSettings();
        try
        {
            return JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(SettingsPath), JsonOpts) ?? new AppSettings();
        }
        catch { return new AppSettings(); }
    }

    public void SaveSettings(AppSettings s) =>
        File.WriteAllText(SettingsPath, JsonSerializer.Serialize(s, JsonOpts));

    private string PathFor(string name)
    {
        foreach (var c in Path.GetInvalidFileNameChars()) name = name.Replace(c, '_');
        return Path.Combine(ProfilesDirectory, name + ".json");
    }
}

public sealed class AppSettings
{
    public string? StartupProfile { get; set; }
    public bool ApplyOnStartup { get; set; }
    public bool StartMinimized { get; set; }
    public int PollIntervalMs { get; set; } = 1000;
    public int HistorySeconds { get; set; } = 120;
    public bool UseMockBackend { get; set; }
    /// <summary>Hardware monitor collapsed, leaving only the control column.</summary>    /// <summary>Last applied profile per GPU (by name), restored into the sliders on launch.</summary>
    public Dictionary<string, string> LastProfileByGpu { get; set; } = new();

    /// <summary>
    /// Highest core voltage seen under load, per GPU. The card's real ceiling doesn't change between
    /// runs, but it can only be learned while something is sampling — and the monitor is closed most
    /// of the time by design. Remembering it stops the ceiling resetting to the V/F table's top,
    /// which overstates it on cards that never reach their own last curve point.
    /// </summary>
    public Dictionary<string, int> ObservedMaxVoltageByGpu { get; set; } = new();

    /// <summary>
    /// Highest core voltage seen per GPU with the boost wound fully open. Together with the stock
    /// figure above this gives the card's real boost headroom, which no vendor call reports and
    /// which otherwise falls back to a constant measured on one 4070 Ti.
    /// </summary>
    public Dictionary<string, int> ObservedMaxBoostedVoltageByGpu { get; set; } = new();

    /// <summary>
    /// Voltage-rail ceilings as first seen on each GPU, in mV, and what Reset restores them to.
    ///
    /// This has to be remembered rather than read: the driver reports no factory default anywhere,
    /// a rail offset survives a reboot, and every new process therefore sees whatever was last
    /// applied and would call that the default. Recorded once, on first sight of a GPU, and never
    /// overwritten — MSVDD ships with an offset already applied (985 mV against a 1035 mV base on a
    /// 5070 Ti), so getting this wrong leaves the rail permanently above stock.
    /// </summary>
    public Dictionary<string, int> NvvddDefaultMaxByGpu { get; set; } = new();
    public Dictionary<string, int> MsvddDefaultMaxByGpu { get; set; } = new();

}
