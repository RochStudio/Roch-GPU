namespace GpuTuner.App.ViewModels;

/// <summary>
/// One numbered profile button in the Afterburner-style profile bar. A slot is just a named
/// profile on disk ("Slot 1" … "Slot 5"), so slots and any legacy named profiles share one store.
/// </summary>
public sealed class ProfileSlot : ObservableObject
{
    public ProfileSlot(int number) => Number = number;

    public int Number { get; }
    /// <summary>Profile name used on disk. Kept stable so the logon task can target a slot.</summary>
    public string Name => $"Slot {Number}";
    public string Label => Number.ToString();

    private bool _occupied;
    public bool Occupied
    {
        get => _occupied;
        set { if (Set(ref _occupied, value)) OnPropertyChanged(nameof(Tip)); }
    }

    /// <summary>True for the slot that was last loaded or saved — the bar highlights it in red.</summary>
    private bool _isActive;
    public bool IsActive { get => _isActive; set => Set(ref _isActive, value); }

    private string _summary = "";
    public string Summary
    {
        get => _summary;
        set { if (Set(ref _summary, value)) OnPropertyChanged(nameof(Tip)); }
    }

    public string Tip => Occupied
        ? $"Slot {Number}\n{Summary}\n\nClick to load and apply · right-click to clear"
        : $"Slot {Number} is empty\n\nPress Save, then this number, to store the current settings";
}
