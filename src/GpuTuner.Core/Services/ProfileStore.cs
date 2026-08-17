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

}
