using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GpuTuner.Core.Models;

namespace GpuTuner.Core.Services;

/// <summary>Durable profile trial journal. Persist before writes; retain pending state if rollback fails.</summary>
public sealed class ProfileRecovery
{
    public sealed class State
    {
        public string DeviceKey { get; set; } = "";
        public TuningProfile? LastConfirmed { get; set; }
        public TuningProfile? Pending { get; set; }
        public Dictionary<string, TuningProfile> Startup { get; set; } = new();
    }
    private readonly string _path;
    private readonly string _key;
    private readonly Func<TuningProfile, IReadOnlyList<string>> _apply;
    private readonly TuningProfile _defaults;
    private State _state;
    private bool _canConfirm;
    public bool HasPending => _state.Pending != null;
    public TuningProfile? LastConfirmed => _state.LastConfirmed?.Clone();

    public ProfileRecovery(string directory, string deviceKey, TuningProfile defaults,
                           Func<TuningProfile, IReadOnlyList<string>> apply)
    {
        _key = deviceKey; _defaults = defaults.Clone(); _apply = apply;
        Directory.CreateDirectory(directory);
        _path = Path.Combine(directory, "recovery-" + Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(deviceKey))) + ".json");
        // Invalid journals must fail closed: do not silently forget a pending unstable tune.
        _state = File.Exists(_path)
            ? JsonSerializer.Deserialize<State>(File.ReadAllText(_path)) ?? throw new IOException("Empty recovery journal")
            : new State { DeviceKey = deviceKey };
        if (_state.DeviceKey != deviceKey) throw new IOException("Recovery journal belongs to a different GPU");
    }

    private void Save(State next)
    {
        string temp = _path + ".tmp";
        using (var file = new FileStream(temp, FileMode.Create, FileAccess.Write, FileShare.None))
        {
            JsonSerializer.Serialize(file, next);
            file.Flush(flushToDisk: true);
        }
        File.Move(temp, _path, overwrite: true);
        _state = next;
    }
    private State Copy() => new()
    {
        DeviceKey = _key, LastConfirmed = _state.LastConfirmed?.Clone(), Pending = _state.Pending?.Clone(),
        Startup = _state.Startup.ToDictionary(p => p.Key, p => p.Value.Clone())
    };
    private static bool Success(IReadOnlyList<string> errors) => errors.Count == 0 || TuningService.OnlyNotes(errors);

    public IReadOnlyList<string> Begin(TuningProfile candidate)
    {
        if (HasPending) throw new InvalidOperationException("Recover the pending trial before applying another profile.");
        _canConfirm = false;
        var next = Copy(); next.Pending = candidate.Clone(); Save(next);
        try { var errors = _apply(candidate.Clone()); _canConfirm = Success(errors); return errors; }
        catch (Exception ex) { return new[] { ex.Message }; }
    }
    public void Confirm()
    {
        if (!HasPending || !_canConfirm) throw new InvalidOperationException("No successful profile trial is active.");
        var next = Copy(); next.LastConfirmed = next.Pending!.Clone(); next.Pending = null; Save(next); _canConfirm = false;
    }
    public IReadOnlyList<string> Revert()
    {
        _canConfirm = false;
        if (!HasPending) return Array.Empty<string>();
        IReadOnlyList<string> errors;
        try { errors = _apply((_state.LastConfirmed ?? _defaults).Clone()); }
        catch (Exception ex) { return new[] { ex.Message }; }
        if (Success(errors)) { var next = Copy(); next.Pending = null; Save(next); _canConfirm = false; }
        return errors;
    }
    public void ApproveStartup(string name)
    {
        if (HasPending || _state.LastConfirmed == null) throw new InvalidOperationException("Confirm a profile trial before enabling startup.");
        var next = Copy(); next.Startup[name] = next.LastConfirmed!.Clone(); Save(next);
    }
    public TuningProfile? StartupProfile(string name) =>
        _state.Startup.TryGetValue(name, out var p) ? p.Clone() : null;
}
