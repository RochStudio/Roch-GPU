using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Windows;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

namespace GpuTuner.App.ViewModels;

public sealed class MainViewModel : ObservableObject
{
    private readonly TuningService _svc;
    private readonly ProfileStore _store;

    public MainViewModel(TuningService svc, ProfileStore store)
    {
        _svc = svc; _store = store;
        Caps = svc.Capabilities;
        Device = svc.Device;
        BackendName = svc.Backend.BackendName;

        // Seed sliders with what the driver currently has.
        var cur = svc.ReadCurrentAsProfile();
        cur.ClampTo(Caps);
        LoadIntoEditor(cur);
        _pendingChanges = false;

        ApplyCommand = new RelayCommand(Apply, () => true);
        ResetCommand = new RelayCommand(Reset);
        SaveProfileCommand = new RelayCommand(SaveProfile, () => !string.IsNullOrWhiteSpace(ProfileNameInput));
        LoadProfileCommand = new RelayCommand(LoadSelectedProfile, () => SelectedProfile != null);
        DeleteProfileCommand = new RelayCommand(DeleteSelectedProfile, () => SelectedProfile != null);
        RevertCommand = new RelayCommand(Revert);
        SlotCommand = new RelayCommand(p => OnSlotClicked(ToSlotNumber(p)));
        NudgeCommand = new RelayCommand(Nudge);
        for (int i = 1; i <= SlotCount; i++) Slots.Add(new ProfileSlot(i));
        RefreshProfiles();
        RefreshSlots();

        var settings = App.Settings;
        _applyOnStartup = settings.ApplyOnStartup && StartupTaskService.Exists();
        _startupProfile = settings.StartupProfile;

        // Light up the slot that was applied last session, so the bar reflects what the card is running.
        if (settings.LastProfileByGpu.TryGetValue(Device.Name, out var lastName))
        {
            var last = Slots.FirstOrDefault(s => s.Name == lastName && s.Occupied);
            if (last != null) SetActiveSlot(last.Number);
        }
    }

    // ------------------------------------------------------------------ static info
    public GpuCapabilities Caps { get; }
    public GpuDevice Device { get; }
    public string BackendName { get; }
    public string VramText => Device.VramMegabytes > 0 ? $"{Device.VramMegabytes / 1024.0:0.#} GB" : "";

    /// <summary>
    /// Driver, vBIOS and VRAM on one line. Fields the vendor library declines to report are left
    /// out entirely rather than shown as an empty label or "0 GB".
    /// </summary>
    public string DeviceLine
    {
        get
        {
            var parts = new List<string>(3);
            if (!string.IsNullOrWhiteSpace(Device.DriverVersion)) parts.Add($"Driver {Device.DriverVersion}");
            if (!string.IsNullOrWhiteSpace(Device.BiosVersion)) parts.Add($"vBIOS {Device.BiosVersion}");
            if (VramText.Length > 0) parts.Add(VramText);
            if (parts.Count == 0) parts.Add(BackendName);
            return string.Join("   ·   ", parts);
        }
    }    public string CoreRangeText => $"{Caps.CoreOffsetMinMhz:+#;-#;0} … {Caps.CoreOffsetMaxMhz:+#;-#;0} MHz";
    public string MemRangeText => Caps.MemoryClockIsAbsolute
        ? $"{Caps.MemoryOffsetMinMhz} … {Caps.MemoryOffsetMaxMhz} MHz (stock {Caps.MemoryClockDefaultMhz})"
        : $"{Caps.MemoryOffsetMinMhz:+#;-#;0} … {Caps.MemoryOffsetMaxMhz:+#;-#;0} MHz";

    // ---------------- which controls this card actually has ----------------

    /// <summary>NVIDIA: one absolute mV slider driven by the V/F curve.</summary>
    public bool IsVoltageAbsolute => Caps.VoltageStyle == VoltageControlStyle.Absolute && Caps.CanSetVoltage;
    /// <summary>AMD: a single signed offset applied to the whole curve.</summary>
    public bool IsVoltageOffset => Caps.VoltageStyle == VoltageControlStyle.Offset;
    /// <summary>
    /// NVIDIA's over-voltage percentage, Afterburner's "Core Voltage (%)". It raises the ceiling the
    /// card may reach above the top of the V/F table; it is a separate lever from the cap, which
    /// holds the core at or below a chosen mV.
    /// </summary>
    public bool HasVoltageBoost => IsVoltageAbsolute && Caps.CanSetVoltageBoost;
    public bool HasCurveEditor => Caps.CanSetVoltageCurve;
    public bool HasTempLimit => Caps.CanSetTempLimit;
    public bool HasZeroRpm => Caps.CanSetZeroRpm;
    public bool HasMemoryTiming => Caps.CanSetMemoryTiming && Caps.MemoryTimingOptions.Count > 0;

    public string MemoryLabel => Caps.MemoryClockIsAbsolute ? "Memory Clock (MHz)" : "Memory Clock (MHz offset)";
    public string CoreLabel => "Core Clock (MHz)";
    public string VoltageLabel => IsVoltageOffset ? "Voltage Offset (mV)" : "Core Voltage (mV)";
    public string PowerLabel => Caps.PowerLimitIsOffset ? "Power Limit (% offset)" : "Power Limit (%)";

    public IReadOnlyList<string> MemoryTimingOptions => Caps.MemoryTimingOptions;
    public string VoltageOffsetRangeText => $"{Caps.VoltageOffsetMinMv} … {Caps.VoltageOffsetMaxMv} mV (0 = stock)";
    public string VoltageBoostRangeText =>
        $"{Caps.VoltageBoostMinPercent} … {Caps.VoltageBoostMaxPercent} % (0 = stock ceiling). " +
        "Raises how far the core may be driven above the top of the V/F table; it does not add curve points.";

    /// <summary>What the boost buys, in the mV the rest of the UI is expressed in.</summary>
    public string VoltageBoostEffectText
    {
        get
        {
            int ceiling = StockCeilingMv, max = Caps.MaxVoltageMv;
            if (VoltageBoost <= 0)
                return ceiling > 0 ? $"Stock ceiling ({ceiling} mV)." : "Stock ceiling.";
            if (ceiling <= 0 || max <= ceiling) return $"+{VoltageBoost} % of the card's boost headroom.";
            int mv = ceiling + (int)Math.Round(VoltageBoost / 100.0 * (max - ceiling));
            return $"+{VoltageBoost} % — lets the core reach {mv} mV (stock {ceiling} mV).";
        }
    }
    public string PowerRangeText => $"{Caps.PowerLimitMinPercent} … {Caps.PowerLimitMaxPercent} % (default {Caps.PowerLimitDefaultPercent})";
    public string TempRangeText => $"{Caps.TempLimitMinC} … {Caps.TempLimitMaxC} °C (default {Caps.TempLimitDefaultC})";

    // ------------------------------------------------------------------ editor values (sliders)
    private int _core, _mem, _power, _temp, _volt, _uv, _vTarget, _fanFixed, _fanModeIndex;
    private bool _pendingChanges;

    // Each numeric property clamps to the driver range on set, so typing "+9999" or dragging past the end is safe,
    // and raises both the "…Text" label and the "…Input" text-box mirror so slider and box stay in sync.
    /// <summary>Core offsets snap to the driver's own 15 MHz grid — see <see cref="ClockStep"/>.</summary>
    private const int CoreStepMhz = ClockStep.CoreMhz;

    public int CoreOffset { get => _core; set { if (SetClamped(ref _core, ClockStep.SnapWithin(value, CoreStepMhz, Caps.CoreOffsetMinMhz, Caps.CoreOffsetMaxMhz), Caps.CoreOffsetMinMhz, Caps.CoreOffsetMaxMhz)) { Dirty(); RaiseVal(nameof(CoreOffset)); } } }
    /// <summary>
    /// Grid the memory value snaps to. An NVIDIA offset goes in 25 MHz steps; an absolute AMD memory
    /// clock is deliberately left alone (1 = no snapping), because its stock value sits wherever the
    /// driver puts it — 2518 MHz would round to 2525 and overclock the card just from reading it.
    /// </summary>
    private int MemorySnapMhz => Caps.MemoryClockIsAbsolute ? 1 : ClockStep.MemoryMhz;

    /// <summary>How far one nudge or slider step moves memory. Bound by the slider too.</summary>
    public int MemoryStepMhz => Caps.MemoryClockIsAbsolute ? 5 : ClockStep.MemoryMhz;

    public int MemoryOffset { get => _mem; set { if (SetClamped(ref _mem, ClockStep.SnapWithin(value, MemorySnapMhz, Caps.MemoryOffsetMinMhz, Caps.MemoryOffsetMaxMhz), Caps.MemoryOffsetMinMhz, Caps.MemoryOffsetMaxMhz)) { Dirty(); RaiseVal(nameof(MemoryOffset)); } } }
    public int PowerLimit { get => _power; set { if (SetClamped(ref _power, value, Caps.PowerLimitMinPercent, Caps.PowerLimitMaxPercent)) { Dirty(); RaiseVal(nameof(PowerLimit)); } } }
    public int TempLimit { get => _temp; set { if (SetClamped(ref _temp, value, Caps.TempLimitMinC, Caps.TempLimitMaxC)) { Dirty(); RaiseVal(nameof(TempLimit)); } } }
    public int VoltageBoost { get => _volt; set { if (SetClamped(ref _volt, value, Caps.VoltageBoostMinPercent, Caps.VoltageBoostMaxPercent)) { Dirty(); RaiseVal(nameof(VoltageBoost)); OnPropertyChanged(nameof(VoltageBoostEffectText)); } } }
    public int VoltageOffset { get => _uv; set { if (SetClamped(ref _uv, value, Caps.VoltageOffsetMinMv, Caps.VoltageOffsetMaxMv)) { Dirty(); RaiseVal(nameof(VoltageOffset)); OnPropertyChanged(nameof(VoltageCapText)); } } }
    /// <summary>Absolute ceiling in mV the core is held at — the "voltage cap" slider. Independent of
    /// <see cref="VoltageBoost"/>, which raises the top of the range this caps within.</summary>
    public int TargetVoltage { get => _vTarget; set { if (SetClamped(ref _vTarget, value, Caps.MinVoltageMv, Math.Max(Caps.MinVoltageMv, Caps.MaxVoltageMv))) { Dirty(); RaiseVal(nameof(TargetVoltage)); OnPropertyChanged(nameof(VoltageCapText)); } } }
    public int FixedFan { get => _fanFixed; set { if (SetClamped(ref _fanFixed, value, Caps.FanMinPercent, Caps.FanMaxPercent)) { Dirty(); RaiseVal(nameof(FixedFan)); } } }

    private bool _zeroRpm = true;
    /// <summary>AMD: let the fans stop entirely at idle.</summary>
    public bool ZeroRpm { get => _zeroRpm; set { if (Set(ref _zeroRpm, value)) Dirty(); } }

    private int _memTiming;
    /// <summary>AMD: index into <see cref="MemoryTimingOptions"/>.</summary>
    public int MemoryTimingIndex { get => _memTiming; set { if (Set(ref _memTiming, value)) Dirty(); } }

    /// <summary>
    /// Step one slider by one increment. Bound to the arrow buttons either side of each slider, so
    /// a value can be dialled in exactly without fighting the mouse — the parameter is
    /// "<c>name:direction</c>", e.g. "core:-1".
    /// </summary>
    public RelayCommand NudgeCommand { get; private set; } = null!;

    private void Nudge(object? parameter)
    {
        if (parameter is not string spec) return;
        int colon = spec.LastIndexOf(':');
        if (colon <= 0) return;
        string name = spec[..colon];
        if (!int.TryParse(spec[(colon + 1)..], out int dir) || dir == 0) return;

        // Step sizes match how coarse each control is: the clocks in the driver's own granularity
        // (core 15 MHz, memory offset 25), voltage in 5s, percentages in 1s.
        switch (name)
        {
            case "volt": TargetVoltage += 5 * dir; break;
            case "voltboost": VoltageBoost += dir; break;   // percent, so one step is one unit
            case "voltoffset": VoltageOffset += 5 * dir; break;
            case "core": CoreOffset += CoreStepMhz * dir; break;
            case "mem": MemoryOffset += MemoryStepMhz * dir; break;
            case "power": PowerLimit += dir; break;
            case "temp": TempLimit += dir; break;
            case "fan": FixedFan += dir; break;
        }
    }

    private bool SetClamped(ref int field, int value, int min, int max)
    {
        value = Math.Clamp(value, min, max);
        return Set(ref field, value);
    }

    // ---- typeable text boxes. Accept "+15", "-20", "60", " +5 " etc. Empty/garbage leaves the value unchanged.
    // Always re-normalize the box (RaiseVal raises "…Input") after a commit: on a good parse the value
    // property already re-raised it, but when the parse fails OR the clamped value equals the current one
    // (Set returns false, so the property's own RaiseVal never runs) the box would otherwise keep the raw
    // text the user typed (e.g. "-5" or "150"). Notifying the target back never re-invokes this setter, so
    // there is no loop.
    public string CoreOffsetInput { get => _core.ToString("+#;-#;0"); set { if (TryParseSigned(value, out var v)) CoreOffset = v; RaiseVal(nameof(CoreOffset)); } }
    public string MemoryOffsetInput
    {
        get => Caps.MemoryClockIsAbsolute ? _mem.ToString() : _mem.ToString("+#;-#;0");
        set { if (TryParseSigned(value, out var v)) MemoryOffset = v; RaiseVal(nameof(MemoryOffset)); }
    }
    public string PowerLimitInput { get => _power.ToString(); set { if (TryParseSigned(value, out var v)) PowerLimit = v; RaiseVal(nameof(PowerLimit)); } }
    public string TempLimitInput { get => _temp.ToString(); set { if (TryParseSigned(value, out var v)) TempLimit = v; RaiseVal(nameof(TempLimit)); } }    public string VoltageOffsetInput { get => _uv.ToString("+#;-#;0"); set { if (TryParseSigned(value, out var v)) VoltageOffset = v; RaiseVal(nameof(VoltageOffset)); OnPropertyChanged(nameof(VoltageCapText)); } }
    public string TargetVoltageInput { get => _vTarget.ToString(); set { if (TryParseSigned(value, out var v)) TargetVoltage = v; RaiseVal(nameof(TargetVoltage)); OnPropertyChanged(nameof(VoltageCapText)); } }
    public string VoltageBoostInput { get => _volt.ToString(); set { if (TryParseSigned(value, out var v)) VoltageBoost = v; RaiseVal(nameof(VoltageBoost)); OnPropertyChanged(nameof(VoltageBoostEffectText)); } }
    public string FixedFanInput { get => _fanFixed.ToString(); set { if (TryParseSigned(value, out var v)) FixedFan = v; RaiseVal(nameof(FixedFan)); } }

    /// <summary>Parse a user-typed offset: leading +/- allowed, whitespace trimmed, trailing units ignored.</summary>
    public static bool TryParseSigned(string? s, out int value)
    {
        value = 0;
        if (string.IsNullOrWhiteSpace(s)) return false;
        s = s.Trim();
        int i = 0; int sign = 1;
        if (s[0] == '+') i = 1;
        else if (s[0] == '-') { sign = -1; i = 1; }
        int start = i;
        while (i < s.Length && char.IsDigit(s[i])) i++;
        if (i == start) return false;                       // no digits
        if (!int.TryParse(s.Substring(start, i - start), out var mag)) return false;
        value = sign * mag;
        return true;
    }

    /// <summary>Raise all bindings tied to one numeric value: the value, its "Text" label and its "Input" box.</summary>
    private void RaiseVal(string prop)
    {
        OnPropertyChanged(prop);
        OnPropertyChanged(prop + "Text");
        OnPropertyChanged(prop + "Input");
    }
    /// <summary>0 = Auto, 1 = Fixed, 2 = Curve</summary>
    public int FanModeIndex
    {
        get => _fanModeIndex;
        set { if (Set(ref _fanModeIndex, value)) { Dirty(); OnPropertyChanged(nameof(IsFixedFan)); OnPropertyChanged(nameof(IsCurveFan)); } }
    }
    public bool IsFixedFan => _fanModeIndex == 1;
    public bool IsCurveFan => _fanModeIndex == 2;

    /// <summary>Owned by the curve editor control; VM keeps a copy for save/apply.</summary>
    public FanCurve EditorCurve { get; private set; } = new();

    public bool PendingChanges { get => _pendingChanges; private set => Set(ref _pendingChanges, value); }
    private void Dirty() => PendingChanges = true;
    public void MarkCurveDirty() => Dirty();

    public string CoreOffsetText => $"{CoreOffset:+#;-#;0} MHz";
    public string MemoryOffsetText => $"{MemoryOffset:+#;-#;0} MHz";
    public string PowerLimitText => $"{PowerLimit} %";
    public string TempLimitText => $"{TempLimit} °C";    public string VoltageOffsetText => $"{VoltageOffset:+#;-#;0} mV";
    public string TargetVoltageText => $"{TargetVoltage} mV";
    /// <summary>What this card really tops out at — the curve's last point is a step below it.</summary>
    public int StockCeilingMv => _svc.StockCeilingMv;
    public string TargetVoltageRangeText =>
        $"{Caps.MinVoltageMv} … {Caps.MaxVoltageMv} mV. Holds the core at or below this voltage; "
        + $"set it at or above the ceiling to cap nothing. This card tops out at {StockCeilingMv} mV "
        + $"(the V/F curve's last point is {Caps.StockMaxVoltageMv} mV; the VID sits a step above it).";
    public string VoltageCapText
    {
        get
        {
            if (!Caps.CanSetVoltage)
                return string.IsNullOrEmpty(Caps.CurveUnavailableReason)
                    ? "voltage control unavailable on this driver"
                    : "voltage control unavailable — " + Caps.CurveUnavailableReason;
            int ceiling = StockCeilingMv;
            // At or above the ceiling this slider isn't capping anything — raising the ceiling is the
            // boost control's job, not this one's.
            if (TargetVoltage >= ceiling) return $"no cap — free to the ceiling ({ceiling} mV)";
            // Say which lever is holding it once applied — the private lock is ignored on some
            // drivers and the clock cap takes over, which changes what you'll see in a monitor.
            string how = _svc.VoltageLockMechanism;
            string via = how is "none" or "n/a" or "" ? "" : $" · via {how}";
            return $"undervolt — capped at {TargetVoltage} mV (stock {ceiling} mV){via}";
        }
    }

    public string FixedFanText => $"{FixedFan} %";
    // ------------------------------------------------------------------ live telemetry
    private GpuTelemetry? _t;
    private int _lastCeiling;
    public GpuTelemetry? Telemetry
    {
        get => _t;
        set
        {
            _t = value;
            OnPropertyChanged(); OnPropertyChanged(nameof(LimitReasonText)); OnPropertyChanged(nameof(LimitIsActive));
            if (_svc.StockCeilingMv != _lastCeiling)
            {
                _lastCeiling = _svc.StockCeilingMv;
                OnPropertyChanged(nameof(StockCeilingMv));
                OnPropertyChanged(nameof(VoltageCapText));
                OnPropertyChanged(nameof(TargetVoltageRangeText));
            }
        }
    }
    // No sample means the monitor is closed and nothing is being polled — say so, rather than
    // reporting "no limiter", which is a claim we haven't measured.
    public string LimitReasonText => _t == null
        ? "Limiter — open the monitor to sample"
        : string.IsNullOrEmpty(_t.LimitReason) || _t.LimitReason == "None"
            ? "No active limiter" : $"Limited by {_t.LimitReason.ToLowerInvariant()}";
    public bool LimitIsActive => _t != null && !string.IsNullOrEmpty(_t.LimitReason) && _t.LimitReason != "None";

    // ------------------------------------------------------------------ status
    private string _status = "Ready";
    public string Status { get => _status; set => Set(ref _status, value); }
    private bool _statusIsError;
    public bool StatusIsError { get => _statusIsError; set => Set(ref _statusIsError, value); }
    public string AppliedSummary
    {
        get
        {
            var p = _svc.AppliedProfile;
            if (p == null) return "Nothing applied this session (driver state shown)";
            // Read back rather than echoing what we asked for — if the card ignored something,
            // this line should show the truth.
            try
            {
                var a = _svc.Backend.ReadTuningState(_svc.GpuIndex);
                // Voltage as an absolute cap, not the old "uv -90 mV" offset: the offset is always 0
                // now that one slider drives both levers, so it told you nothing. Name the lever too —
                // the private lock and the clock cap look different in a monitoring tool.
                string volts;
                if (Caps.VoltageStyle == VoltageControlStyle.Offset)
                {
                    volts = a.VoltageOffsetMv == 0 ? "stock" : $"{a.VoltageOffsetMv:+#;-#;0} mV";
                }
                else
                {
                    int cap = _svc.Backend.ReadVoltageLockMv(_svc.GpuIndex);
                    volts = cap > 0
                        ? $"{cap} mV" + (_svc.VoltageLockMechanism is "none" or "n/a" or "" ? "" : $" ({_svc.VoltageLockMechanism})")
                        : a.VoltageBoostPercent > 0 ? $"boost +{a.VoltageBoostPercent}%" : "stock";
                }

                // Memory is an absolute clock on AMD and an offset on NVIDIA; a temperature limit the
                // driver owns is not worth a column at all.
                string mem = Caps.MemoryClockIsAbsolute ? $"{a.MemoryOffsetMhz} MHz" : $"{a.MemoryOffsetMhz:+#;-#;0}";
                string temp = Caps.CanSetTempLimit ? $" · temp {a.TempLimitC}°C" : "";
                string extras = "";
                if (Caps.CanSetMemoryTiming && a.MemoryTimingLevel > 0 && a.MemoryTimingLevel < Caps.MemoryTimingOptions.Count)
                    extras += $" · {Caps.MemoryTimingOptions[a.MemoryTimingLevel].ToLowerInvariant()}";
                if (Caps.CanSetZeroRpm && !a.ZeroRpm) extras += " · zero-rpm off";

                return $"On GPU: volt {volts} · core {a.CoreOffsetMhz:+#;-#;0} · mem {mem} · " +
                       $"power {a.PowerLimitPercent}%{temp} · fan {(a.FanManual ? a.FanPercent + "%" : "auto")}{extras}";
            }
            catch
            {
                return $"Applied: core {p.CoreOffsetMhz:+#;-#;0} · mem {p.MemoryOffsetMhz:+#;-#;0} · power {p.PowerLimitPercent}% · temp {p.TempLimitC}°C · fan {p.FanMode}";
            }
        }
    }

    // ------------------------------------------------------------------ profile slots (Afterburner bar)
    public const int SlotCount = 5;

    /// <summary>The five numbered buttons. Clicking one loads + applies it; arming Save first stores into it.</summary>
    public ObservableCollection<ProfileSlot> Slots { get; } = new();

    private bool _saveArmed;
    /// <summary>
    /// Afterburner's two-step save: press Save, then a number. Nothing is written until the number is
    /// clicked, so an armed Save can be cancelled by pressing Save again.
    /// </summary>
    public bool SaveArmed
    {
        get => _saveArmed;
        set
        {
            if (!Set(ref _saveArmed, value)) return;
            if (value) { Status = "Pick a slot (1-5) to store the current settings"; StatusIsError = false; }
        }
    }

    public RelayCommand SlotCommand { get; }

    private static int ToSlotNumber(object? p) =>
        p is int i ? i : int.TryParse(p?.ToString(), out var n) ? n : 0;

    private void OnSlotClicked(int number)
    {
        var slot = Slots.FirstOrDefault(s => s.Number == number);
        if (slot == null) return;

        if (SaveArmed)
        {
            _store.Save(BuildProfileFromEditor(slot.Name));
            SaveArmed = false;
            RefreshProfiles();
            RefreshSlots();
            SetActiveSlot(number);
            SelectedProfile = slot.Name;
            Status = $"Saved current settings to slot {number}"; StatusIsError = false;
            return;
        }

        var p = _store.Load(slot.Name);
        if (p == null)
        {
            Status = $"Slot {number} is empty — press Save, then {number}, to store the current settings";
            StatusIsError = true;
            return;
        }

        p.ClampTo(Caps);
        LoadIntoEditor(p);
        SelectedProfile = slot.Name;
        SetActiveSlot(number);
        Apply();   // Afterburner applies the moment you click a slot
        if (!StatusIsError) { Status = $"Slot {number} applied at {DateTime.Now:HH:mm:ss}"; }
    }

    /// <summary>Right-click on a slot. Wipes the stored profile; the button goes back to dim/empty.</summary>
    public void ClearSlot(int number)
    {
        var slot = Slots.FirstOrDefault(s => s.Number == number);
        if (slot is not { Occupied: true }) return;
        if (MessageBox.Show($"Clear slot {number}?", "Roch GPU OC",
                MessageBoxButton.YesNo, MessageBoxImage.Question) != MessageBoxResult.Yes) return;

        _store.Delete(slot.Name);
        if (StartupProfile == slot.Name) { StartupProfile = null; ApplyOnStartup = false; }
        if (SelectedProfile == slot.Name) SelectedProfile = null;
        RefreshProfiles();
        RefreshSlots();
        if (slot.IsActive) SetActiveSlot(0);
        Status = $"Slot {number} cleared"; StatusIsError = false;
    }

    private void SetActiveSlot(int number)
    {
        foreach (var s in Slots) s.IsActive = s.Number == number;
    }

    /// <summary>Re-read which slots have a profile behind them, and summarise each for its tooltip.</summary>
    public void RefreshSlots()
    {
        foreach (var s in Slots)
        {
            TuningProfile? p = null;
            try { p = _store.Load(s.Name); } catch { /* unreadable slot = empty */ }
            s.Occupied = p != null;
            s.Summary = p == null ? "" : DescribeProfile(p);
        }
    }

    private static string DescribeProfile(TuningProfile p)
    {
        var bits = new List<string>
        {
            $"core {p.CoreOffsetMhz:+#;-#;0} MHz",
            $"mem {p.MemoryOffsetMhz:+#;-#;0} MHz",
            $"power {p.PowerLimitPercent}%",
            $"temp {p.TempLimitC} °C"
        };
        if (p.TargetVoltageMv > 0) bits.Insert(0, $"{p.TargetVoltageMv} mV");
        bits.Add(p.FanMode switch
        {
            FanMode.Fixed => $"fan {p.FixedFanPercent}%",
            FanMode.Curve => "fan curve",
            _ => "fan auto"
        });
        return string.Join(" · ", bits);
    }

    // ------------------------------------------------------------------ profiles
    public ObservableCollection<string> Profiles { get; } = new();
    private string? _selectedProfile;
    public string? SelectedProfile
    {
        get => _selectedProfile;
        set { if (Set(ref _selectedProfile, value) && value != null) ProfileNameInput = value; }
    }
    private string _profileNameInput = "";
    public string ProfileNameInput { get => _profileNameInput; set => Set(ref _profileNameInput, value); }

    private bool _applyOnStartup;
    public bool ApplyOnStartup
    {
        get => _applyOnStartup;
        set { if (Set(ref _applyOnStartup, value)) UpdateStartupTask(); }
    }
    private string? _startupProfile;
    public string? StartupProfile
    {
        get => _startupProfile;
        set { if (Set(ref _startupProfile, value)) { App.Settings.StartupProfile = value; if (_applyOnStartup) UpdateStartupTask(); } }
    }

    public RelayCommand ApplyCommand { get; }
    public RelayCommand ResetCommand { get; }
    public RelayCommand SaveProfileCommand { get; }
    public RelayCommand LoadProfileCommand { get; }
    public RelayCommand DeleteProfileCommand { get; }
    public RelayCommand RevertCommand { get; }

    // ------------------------------------------------------------------ actions
    public TuningProfile BuildProfileFromEditor(string name) => new()
    {
        Name = name,
        GpuName = Device.Name,
        CoreOffsetMhz = CoreOffset,
        MemoryOffsetMhz = MemoryOffset,
        PowerLimitPercent = PowerLimit,
        TempLimitC = TempLimit,
        VoltageBoostPercent = VoltageBoost,
        VoltageOffsetMv = VoltageOffset,
        TargetVoltageMv = TargetVoltage,
        ZeroRpm = ZeroRpm,
        MemoryTimingLevel = MemoryTimingIndex,
        FanMode = (FanMode)FanModeIndex,
        FixedFanPercent = FixedFan,
        FanCurve = EditorCurve.Clone()
    };

    public void LoadIntoEditor(TuningProfile p)
    {
        CoreOffset = p.CoreOffsetMhz;
        MemoryOffset = p.MemoryOffsetMhz;
        PowerLimit = p.PowerLimitPercent;
        TempLimit = p.TempLimitC;
        VoltageBoost = p.VoltageBoostPercent;
        VoltageOffset = p.VoltageOffsetMv;
        TargetVoltage = p.TargetVoltageMv > 0 ? p.TargetVoltageMv : StockCeilingMv;
        ZeroRpm = p.ZeroRpm;
        MemoryTimingIndex = p.MemoryTimingLevel;
        FanModeIndex = (int)p.FanMode;
        FixedFan = p.FixedFanPercent;
        EditorCurve = p.FanCurve.Clone();
        OnPropertyChanged(nameof(EditorCurve));
        RaiseTexts();
    }

    private void Apply()
    {
        var p = BuildProfileFromEditor(SelectedProfile ?? "Session");
        var errs = _svc.Apply(p);
        if (errs.Count == 0)
        {
            Status = $"Applied at {DateTime.Now:HH:mm:ss}"; StatusIsError = false; PendingChanges = false;
            App.Settings.LastProfileByGpu[Device.Name] = p.Name;
        }
        else
        {
            Status = string.Join("  |  ", errs); StatusIsError = true;
        }
        OnPropertyChanged(nameof(VoltageCapText));   // the mechanism is only known after the write
        OnPropertyChanged(nameof(AppliedSummary));
    }

    private void Reset()
    {
        try
        {
            _svc.ResetToDefaults();
            LoadIntoEditor(TuningProfile.Stock(Caps, Device.Name));
            PendingChanges = false;
            Status = "Reset to driver defaults"; StatusIsError = false;
        }
        catch (Exception e) { Status = e.Message; StatusIsError = true; }
        OnPropertyChanged(nameof(AppliedSummary));
    }

    private void Revert()
    {
        var p = _svc.AppliedProfile ?? _svc.ReadCurrentAsProfile();
        LoadIntoEditor(p);
        PendingChanges = false;
        Status = "Sliders reverted to applied values"; StatusIsError = false;
    }

    private void SaveProfile()
    {
        var name = ProfileNameInput.Trim();
        if (string.IsNullOrEmpty(name)) return;
        _store.Save(BuildProfileFromEditor(name));
        RefreshProfiles();
        SelectedProfile = name;
        Status = $"Saved profile '{name}'"; StatusIsError = false;
    }

    private void LoadSelectedProfile()
    {
        if (SelectedProfile == null) return;
        var p = _store.Load(SelectedProfile);
        if (p == null) { Status = "Profile not found"; StatusIsError = true; RefreshProfiles(); return; }
        if (!string.IsNullOrEmpty(p.GpuName) && p.GpuName != Device.Name)
            Status = $"Note: profile was made for {p.GpuName}, values will be clamped to this GPU's ranges";
        else { Status = $"Loaded '{p.Name}' — press Apply to send to GPU"; StatusIsError = false; }
        p.ClampTo(Caps);
        LoadIntoEditor(p);
        PendingChanges = true;
    }

    private void DeleteSelectedProfile()
    {
        if (SelectedProfile == null) return;
        if (MessageBox.Show($"Delete profile '{SelectedProfile}'?", "Roch GPU OC", MessageBoxButton.YesNo, MessageBoxImage.Question) != MessageBoxResult.Yes) return;
        _store.Delete(SelectedProfile);
        if (StartupProfile == SelectedProfile) { StartupProfile = null; ApplyOnStartup = false; }
        RefreshProfiles();
    }

    public void RefreshProfiles()
    {
        var keep = SelectedProfile;
        Profiles.Clear();
        foreach (var n in _store.ListProfileNames()) Profiles.Add(n);
        SelectedProfile = Profiles.Contains(keep ?? "") ? keep : null;
        OnPropertyChanged(nameof(Profiles));
    }

    private void UpdateStartupTask()
    {
        App.Settings.ApplyOnStartup = _applyOnStartup;
        try
        {
            if (_applyOnStartup)
            {
                if (string.IsNullOrEmpty(StartupProfile))
                {
                    Status = "Pick a startup profile first"; StatusIsError = true;
                    _applyOnStartup = false; OnPropertyChanged(nameof(ApplyOnStartup));
                    return;
                }
                var exe = Environment.ProcessPath ?? throw new InvalidOperationException("Cannot resolve exe path");
                // Stay resident (tray) so fan curves keep running; use --exit if you only want clocks/limits.
                StartupTaskService.Register(exe, StartupProfile, stayResident: true);
                Status = $"Startup task registered for '{StartupProfile}'"; StatusIsError = false;
            }
            else
            {
                StartupTaskService.Unregister();
                Status = "Startup task removed"; StatusIsError = false;
            }
        }
        catch (Exception e) { Status = "Startup task: " + e.Message; StatusIsError = true; }
        App.Store.SaveSettings(App.Settings);
    }

    private void RaiseTexts()
    {
        foreach (var p in new[] { nameof(CoreOffset), nameof(MemoryOffset), nameof(PowerLimit),
                                  nameof(TempLimit), nameof(VoltageBoost), nameof(VoltageOffset), nameof(TargetVoltage), nameof(FixedFan) })
            RaiseVal(p);
        OnPropertyChanged(nameof(VoltageCapText));
        OnPropertyChanged(nameof(VoltageBoostEffectText));
    }
}
