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

        // Seed the measured ceilings BEFORE loading the editor. The cap slider defaults to the stock
        // ceiling, and until an observation is in hand that falls back to the V/F table's top — on a
        // card whose table runs 200 mV past what it reaches, the slider opened at a voltage the card
        // cannot select while the caption underneath correctly read the measured one.
        var settings = App.Settings;
        if (settings.ObservedMaxVoltageByGpu.TryGetValue(svc.Device.Name, out var seenMv))
            svc.SeedObservedMaxVoltage(seenMv);
        if (settings.ObservedMaxBoostedVoltageByGpu.TryGetValue(svc.Device.Name, out var seenBoostMv))
            svc.SeedObservedMaxBoostedVoltage(seenBoostMv);

        // Rail ceilings: record them the first time this GPU is seen, then carry that figure
        // forward. Reset has nothing else to restore them to — see AppSettings for why.
        bool railsRecorded = false;
        if (Caps.CanSetVoltageRail && Caps.VoltageRailStockMaxMv > 0 &&
            !settings.NvvddDefaultMaxByGpu.ContainsKey(svc.Device.Name))
        { settings.NvvddDefaultMaxByGpu[svc.Device.Name] = Caps.VoltageRailStockMaxMv; railsRecorded = true; }
        if (Caps.CanSetMsvddRail && Caps.MsvddRailStockMaxMv > 0 &&
            !settings.MsvddDefaultMaxByGpu.ContainsKey(svc.Device.Name))
        { settings.MsvddDefaultMaxByGpu[svc.Device.Name] = Caps.MsvddRailStockMaxMv; railsRecorded = true; }
        settings.NvvddDefaultMaxByGpu.TryGetValue(svc.Device.Name, out int nvDefault);
        settings.MsvddDefaultMaxByGpu.TryGetValue(svc.Device.Name, out int msDefault);
        svc.SeedRailDefaults(nvDefault, msDefault);
        if (railsRecorded) store.SaveSettings(settings);

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
        EnableXocCommand = new RelayCommand(() => SetXoc(true));
        DisableXocCommand = new RelayCommand(() => SetXoc(false));
        SlotCommand = new RelayCommand(p => OnSlotClicked(ToSlotNumber(p)));
        NudgeCommand = new RelayCommand(Nudge);
        for (int i = 1; i <= SlotCount; i++) Slots.Add(new ProfileSlot(i));
        RefreshProfiles();
        RefreshSlots();

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
    /// <summary>NVVDD: the core rail's own ceiling, a separate lever from the boost and the cap.</summary>
    public bool HasVoltageRail => Caps.CanSetVoltageRail;
    /// <summary>MSVDD: the rail feeding the crossbar, SYS and video domains.</summary>
    public bool HasMsvddRail => Caps.CanSetMsvddRail;
    /// <summary>The interconnect clock, which no public NVAPI surface exposes.</summary>
    public bool HasXbar => Caps.CanSetXbarOffset;
    /// <summary>Whether the card exposes any of the gated levers, and so whether the XOC button appears.</summary>
    public bool HasXoc => HasVoltageRail || HasMsvddRail || HasXbar;
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

    // No caption under the boost slider: it used to predict the voltage the boost would reach, which
    // meant stating a per-card figure derived from VoltageBoostHeadroomMv — a constant measured on
    // one 4070 Ti. On a 5070 Ti that overstates the headroom threefold, and NVAPI exposes no way to
    // ask. Better to show nothing than a number invented for a different card.

    public string PowerRangeText => $"{Caps.PowerLimitMinPercent} … {Caps.PowerLimitMaxPercent} % (default {Caps.PowerLimitDefaultPercent})";
    public string TempRangeText => $"{Caps.TempLimitMinC} … {Caps.TempLimitMaxC} °C (default {Caps.TempLimitDefaultC})";

    // ------------------------------------------------------------------ editor values (sliders)
    private int _core, _mem, _power, _temp, _volt, _uv, _vTarget, _rail, _railFloor, _msvdd, _msvddFloor, _xbar, _fanFixed, _fanModeIndex;
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
    /// <summary>Over-voltage percentage. Snaps to the 5% grid — see <see cref="ClockStep.VoltageBoostPercent"/>.</summary>
    private const int VoltageBoostStepPercent = ClockStep.VoltageBoostPercent;
    public int VoltageBoost { get => _volt; set { if (SetClamped(ref _volt, ClockStep.SnapWithin(value, VoltageBoostStepPercent, Caps.VoltageBoostMinPercent, Caps.VoltageBoostMaxPercent), Caps.VoltageBoostMinPercent, Caps.VoltageBoostMaxPercent)) { Dirty(); RaiseVal(nameof(VoltageBoost)); } } }
    public int VoltageOffset { get => _uv; set { if (SetClamped(ref _uv, value, Caps.VoltageOffsetMinMv, Caps.VoltageOffsetMaxMv)) { Dirty(); RaiseVal(nameof(VoltageOffset)); OnPropertyChanged(nameof(VoltageCapText)); } } }
    /// <summary>Absolute ceiling in mV the core is held at — the "voltage cap" slider. Independent of
    /// <see cref="VoltageBoost"/>, which raises the top of the range this caps within.</summary>
    public int TargetVoltage { get => _vTarget; set { if (SetClamped(ref _vTarget, value, Caps.MinVoltageMv, Math.Max(Caps.MinVoltageMv, BoostCeilingMv))) { Dirty(); RaiseVal(nameof(TargetVoltage)); OnPropertyChanged(nameof(VoltageCapText)); } } }
    /// <summary>
    /// Ceiling of the core voltage rail, in mV. Raising it lets the card select voltages above its
    /// stock maximum — the boost and the cap both operate underneath whatever this allows.
    /// </summary>
    public int VoltageRailMax { get => _rail; set { if (SetClamped(ref _rail, value, Caps.VoltageRailMinMv, Math.Max(Caps.VoltageRailMinMv, Caps.VoltageRailMaxMv))) { Dirty(); RaiseVal(nameof(VoltageRailMax)); OnPropertyChanged(nameof(VoltageRailRangeText)); OnPropertyChanged(nameof(BoostCeilingMv)); } } }

    /// <summary>Floor of the core rail: the lowest voltage it may drop to.</summary>
    public int VoltageRailFloor { get => _railFloor; set { if (SetClamped(ref _railFloor, value, Caps.VoltageRailFloorMinMv, Math.Max(Caps.VoltageRailFloorMinMv, Caps.VoltageRailFloorMaxMv))) { Dirty(); RaiseVal(nameof(VoltageRailFloor)); OnPropertyChanged(nameof(VoltageRailRangeText)); } } }

    /// <summary>Floor of the MSVDD rail.</summary>
    public int MsvddRailFloor { get => _msvddFloor; set { if (SetClamped(ref _msvddFloor, value, Caps.MsvddRailFloorMinMv, Math.Max(Caps.MsvddRailFloorMinMv, Caps.MsvddRailFloorMaxMv))) { Dirty(); RaiseVal(nameof(MsvddRailFloor)); OnPropertyChanged(nameof(MsvddRangeText)); } } }

    /// <summary>Ceiling of the MSVDD rail, in mV. Separate supply from NVVDD.</summary>
    public int MsvddRailMax { get => _msvdd; set { if (SetClamped(ref _msvdd, value, Caps.MsvddRailMinMv, Math.Max(Caps.MsvddRailMinMv, Caps.MsvddRailMaxMv))) { Dirty(); RaiseVal(nameof(MsvddRailMax)); OnPropertyChanged(nameof(MsvddRangeText)); } } }

    /// <summary>Crossbar clock offset in MHz. Snaps to the same 15 MHz grid as the core clock.</summary>
    public int XbarOffset { get => _xbar; set { if (SetClamped(ref _xbar, ClockStep.SnapWithin(value, CoreStepMhz, Caps.XbarOffsetMinMhz, Caps.XbarOffsetMaxMhz), Caps.XbarOffsetMinMhz, Caps.XbarOffsetMaxMhz)) { Dirty(); RaiseVal(nameof(XbarOffset)); } } }

    /// <summary>Fixed fan duty. Snaps to the 5% grid — see <see cref="ClockStep.FanPercent"/>.</summary>
    private const int FanStepPercent = ClockStep.FanPercent;

    public int FixedFan { get => _fanFixed; set { if (SetClamped(ref _fanFixed, ClockStep.SnapWithin(value, FanStepPercent, Caps.FanMinPercent, Caps.FanMaxPercent), Caps.FanMinPercent, Caps.FanMaxPercent)) { Dirty(); RaiseVal(nameof(FixedFan)); } } }

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
            case "voltboost": VoltageBoost += VoltageBoostStepPercent * dir; break;
            case "voltoffset": VoltageOffset += 5 * dir; break;
            case "core": CoreOffset += CoreStepMhz * dir; break;
            case "mem": MemoryOffset += MemoryStepMhz * dir; break;
            case "power": PowerLimit += dir; break;
            case "temp": TempLimit += dir; break;
            case "rail": VoltageRailMax += 5 * dir; break;
            case "msvdd": MsvddRailMax += 5 * dir; break;
            case "railfloor": VoltageRailFloor += 5 * dir; break;
            case "msvddfloor": MsvddRailFloor += 5 * dir; break;
            case "xbar": XbarOffset += CoreStepMhz * dir; break;
            case "fan": FixedFan += FanStepPercent * dir; break;
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
    public string VoltageBoostInput { get => _volt.ToString(); set { if (TryParseSigned(value, out var v)) VoltageBoost = v; RaiseVal(nameof(VoltageBoost)); } }
    public string VoltageRailFloorInput { get => _railFloor.ToString(); set { if (TryParseSigned(value, out var v)) VoltageRailFloor = v; RaiseVal(nameof(VoltageRailFloor)); } }
    public string MsvddRailFloorInput { get => _msvddFloor.ToString(); set { if (TryParseSigned(value, out var v)) MsvddRailFloor = v; RaiseVal(nameof(MsvddRailFloor)); } }
    public string MsvddRailMaxInput { get => _msvdd.ToString(); set { if (TryParseSigned(value, out var v)) MsvddRailMax = v; RaiseVal(nameof(MsvddRailMax)); } }
    public string XbarOffsetInput { get => _xbar.ToString("+#;-#;0"); set { if (TryParseSigned(value, out var v)) XbarOffset = v; RaiseVal(nameof(XbarOffset)); } }
    public string VoltageRailMaxInput { get => _rail.ToString(); set { if (TryParseSigned(value, out var v)) VoltageRailMax = v; RaiseVal(nameof(VoltageRailMax)); } }
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

    /// <summary>
    /// The XOC gate. Not a pending edit like the sliders are: Enable and Disable write the hardware
    /// on the spot, the way mVolt+ does, so there is nothing left over for Apply to send.
    /// </summary>
    public bool XocEnabled
    {
        get => _xocEnabled;
        private set { if (Set(ref _xocEnabled, value)) { OnPropertyChanged(nameof(XocDisabled)); OnPropertyChanged(nameof(XocStatusText)); } }
    }
    private bool _xocEnabled;
    /// <summary>For the Enable button, which is the one to offer while the gate is shut.</summary>
    public bool XocDisabled => !_xocEnabled;
    public string XocStatusText => _xocEnabled
        ? "Enabled - the rails and crossbar below are live on the card."
        : "Disabled - the card is running the driver's own rail and crossbar values.";

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

    /// <summary>Top of the cap slider: the highest voltage the boost can actually reach here.</summary>
    public int BoostCeilingMv => _svc.BoostCeilingMv;

    public string TargetVoltageRangeText =>
        $"{Caps.MinVoltageMv} … {BoostCeilingMv} mV. Holds the core at or below this voltage; "
        + $"set it at or above the ceiling to cap nothing. "
        + (StockCeilingMv < Caps.StockMaxVoltageMv
            ? $"This card has not been seen above {StockCeilingMv} mV under load, against a V/F curve "
              + $"whose last point is {Caps.StockMaxVoltageMv} mV — a cap above {StockCeilingMv} mV caps nothing."
            : $"This card tops out at {StockCeilingMv} mV (the V/F curve's last point is "
              + $"{Caps.StockMaxVoltageMv} mV; the VID sits a step above it).");
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

    public string VoltageRailMaxText => Caps.VoltageRailStockMaxMv > 0 && VoltageRailMax == Caps.VoltageRailStockMaxMv
        ? $"current ceiling ({VoltageRailMax} mV)"
        : $"{VoltageRailMax} mV — {VoltageRailMax - Caps.VoltageRailStockMaxMv:+#;-#;0} mV on the core rail's ceiling";
    public string VoltageRailMaxRangeText =>
        $"{Caps.VoltageRailMinMv} … {Caps.VoltageRailMaxMv} mV (stock {Caps.VoltageRailStockMaxMv} mV). Moves the core "
        + "rail's own ceiling. The driver accepts more than this range offers; it is narrowed to the part worth using.";
    // No "seen" figure beside the MSVDD ceiling, though it would be the useful thing to show: the
    // rail sits well under its roof in normal use. The obvious source is wrong — the rail status
    // entry's +0x04 field reads identically for both rails (0 of 14 samples under load differed)
    // while HWiNFO shows MSVDD about 45 mV below the core, so it is not a per-rail voltage and
    // quoting it here would label the core's reading as MSVDD. The real source looks to be the ADC
    // family (0x43D9B26A), which returns an empty structure until its mask is pre-filled.
    public string MsvddRailMaxText => Caps.MsvddRailStockMaxMv > 0 && MsvddRailMax == Caps.MsvddRailStockMaxMv
        ? $"current ceiling ({MsvddRailMax} mV)"
        : $"{MsvddRailMax} mV on the MSVDD rail";
    public string MsvddRailMaxRangeText =>
        $"{Caps.MsvddRailMinMv} ... {Caps.MsvddRailMaxMv} mV (stock {Caps.MsvddRailStockMaxMv} mV). Feeds the crossbar, "
        + "SYS and video domains rather than the shader core.";
    public string VoltageRailFloorText => Caps.VoltageRailStockFloorMv > 0 && VoltageRailFloor == Caps.VoltageRailStockFloorMv
        ? $"stock floor ({VoltageRailFloor} mV)"
        : $"{VoltageRailFloor} mV floor on the core rail";
    public string VoltageRailFloorRangeText =>
        $"{Caps.VoltageRailFloorMinMv} ... {Caps.VoltageRailFloorMaxMv} mV (stock {Caps.VoltageRailStockFloorMv} mV). "
        + "The lowest the rail may drop to; raising it holds more voltage at idle as well as under load.";
    public string MsvddRailFloorText => Caps.MsvddRailStockFloorMv > 0 && MsvddRailFloor == Caps.MsvddRailStockFloorMv
        ? $"stock floor ({MsvddRailFloor} mV)"
        : $"{MsvddRailFloor} mV floor on the MSVDD rail";
    public string MsvddRailFloorRangeText =>
        $"{Caps.MsvddRailFloorMinMv} ... {Caps.MsvddRailFloorMaxMv} mV (stock {Caps.MsvddRailStockFloorMv} mV).";
    /// <summary>One line for the whole rail: both ends, and whether either has been moved.</summary>
    public string VoltageRailRangeText
    {
        get
        {
            bool stock = VoltageRailMax == Caps.VoltageRailStockMaxMv && VoltageRailFloor == Caps.VoltageRailStockFloorMv;
            return $"{VoltageRailFloor} - {VoltageRailMax} mV" + (stock ? " (stock)" : "");
        }
    }
    public string MsvddRangeText
    {
        get
        {
            bool stock = MsvddRailMax == Caps.MsvddRailStockMaxMv && MsvddRailFloor == Caps.MsvddRailStockFloorMv;
            return $"{MsvddRailFloor} - {MsvddRailMax} mV" + (stock ? " (stock)" : "");
        }
    }
    public string XbarOffsetText => $"{XbarOffset:+#;-#;0} MHz on the interconnect clock";
    public string XbarOffsetRangeText =>
        $"{Caps.XbarOffsetMinMhz:+#;-#;0} … {Caps.XbarOffsetMaxMhz:+#;-#;0} MHz. Offsets the crossbar, which no public "
        + "NVAPI surface exposes; the GPU's own frequency counter is used to verify the write landed.";
    public string FixedFanText => $"{FixedFan} %";
    // ------------------------------------------------------------------ live telemetry
    private GpuTelemetry? _t;
    private int _lastCeiling, _lastBoostCeiling;
    public GpuTelemetry? Telemetry
    {
        get => _t;
        set
        {
            _t = value;
            OnPropertyChanged(); OnPropertyChanged(nameof(LimitReasonText)); OnPropertyChanged(nameof(LimitIsActive));
            if (_svc.StockCeilingMv != _lastCeiling || _svc.BoostCeilingMv != _lastBoostCeiling)
            {
                _lastCeiling = _svc.StockCeilingMv;
                _lastBoostCeiling = _svc.BoostCeilingMv;
                OnPropertyChanged(nameof(StockCeilingMv));
                OnPropertyChanged(nameof(BoostCeilingMv));
                OnPropertyChanged(nameof(VoltageCapText));
                OnPropertyChanged(nameof(TargetVoltageRangeText));

                // Only moves when the card shows us a voltage it has never reached before, so this
                // writes rarely rather than on every sample.
                if (_svc.ObservedMaxVoltageMv > 0)
                    App.Settings.ObservedMaxVoltageByGpu[Device.Name] = _svc.ObservedMaxVoltageMv;
                if (_svc.ObservedMaxBoostedVoltageMv > 0)
                    App.Settings.ObservedMaxBoostedVoltageByGpu[Device.Name] = _svc.ObservedMaxBoostedVoltageMv;
                if (_svc.ObservedMaxVoltageMv > 0 || _svc.ObservedMaxBoostedVoltageMv > 0)
                    App.Store.SaveSettings(App.Settings);
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
    private string _appliedSummary = "Nothing applied this session (driver state shown)";

    /// <summary>
    /// What the card is actually carrying, read back rather than echoed. Computed when something is
    /// applied and cached: it costs two driver calls and roughly 35 ms, the XAML binds it twice
    /// (label and tooltip), and a property getter that stalls the UI thread that long is a trap for
    /// whoever next adds a binding to it.
    /// </summary>
    public string AppliedSummary => _appliedSummary;

    private void RefreshAppliedSummary()
    {
        _appliedSummary = BuildAppliedSummary();
        OnPropertyChanged(nameof(AppliedSummary));
    }

    private string BuildAppliedSummary()
    {
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
    public RelayCommand EnableXocCommand { get; }
    public RelayCommand DisableXocCommand { get; }

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
        VoltageRailMaxMv = HasVoltageRail ? VoltageRailMax : 0,
        MsvddRailMaxMv = HasMsvddRail ? MsvddRailMax : 0,
        VoltageRailFloorMv = HasVoltageRail ? VoltageRailFloor : 0,
        MsvddRailFloorMv = HasMsvddRail ? MsvddRailFloor : 0,
        XbarOffsetMhz = HasXbar ? XbarOffset : 0,
        XocEnabled = XocEnabled,
        // The cap has no slider any more — the curve editor's flatten owns it. Carry whatever lock is
        // actually on the card so pressing Apply here preserves a flatten set over there instead of
        // overwriting it with a value this window last read at startup.
        TargetVoltageMv = Math.Max(0, _svc.Backend.ReadVoltageLockMv(_svc.GpuIndex)),
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
        VoltageRailMax = p.VoltageRailMaxMv > 0 ? p.VoltageRailMaxMv : Caps.VoltageRailStockMaxMv;
        MsvddRailMax = p.MsvddRailMaxMv > 0 ? p.MsvddRailMaxMv : Caps.MsvddRailStockMaxMv;
        VoltageRailFloor = p.VoltageRailFloorMv > 0 ? p.VoltageRailFloorMv : Caps.VoltageRailStockFloorMv;
        MsvddRailFloor = p.MsvddRailFloorMv > 0 ? p.MsvddRailFloorMv : Caps.MsvddRailStockFloorMv;
        XbarOffset = p.XbarOffsetMhz;
        XocEnabled = p.XocEnabled;
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
        // Notes say what the apply did differently, not that it failed — showing them in the error
        // colour would make a clean apply look broken.
        if (errs.Count == 0 || TuningService.OnlyNotes(errs))
        {
            Status = errs.Count == 0
                ? $"Applied at {DateTime.Now:HH:mm:ss}"
                : string.Join("  |  ", errs);
            StatusIsError = false; PendingChanges = false;
            App.Settings.LastProfileByGpu[Device.Name] = p.Name;
        }
        else
        {
            Status = string.Join("  |  ", errs); StatusIsError = true;
        }
        OnPropertyChanged(nameof(VoltageCapText));   // the mechanism is only known after the write
        OnPropertyChanged(nameof(BoostCeilingMv));  // a raised rail ceiling lifts the cap slider's top
        RefreshAppliedSummary();
    }

    /// <summary>
    /// Enable / Disable, mVolt+ style: one click writes the rails and crossbar, or puts them back.
    /// Deliberately narrow - it never touches clocks, power, temp or fan, so arming the gate cannot
    /// smuggle a half-finished slider edit onto the card behind the user's back.
    /// </summary>
    private void SetXoc(bool on)
    {
        var errs = _svc.SetXocEnabled(BuildProfileFromEditor(SelectedProfile ?? "Session"), on);
        // The gate only counts as open if the writes behind it landed; leaving it showing "Enabled"
        // after a failed rail write would be the tool lying about the state of the card.
        bool ok = errs.Count == 0 || TuningService.OnlyNotes(errs);
        XocEnabled = on && ok;
        Status = errs.Count > 0
            ? string.Join("  |  ", errs)
            : on ? "XOC enabled" : "XOC disabled - rails and crossbar back to driver defaults";
        StatusIsError = !ok;
        if (!on && ok)
        {
            // Show what the card is actually carrying now rather than the values that were armed.
            VoltageRailMax = Caps.VoltageRailStockMaxMv;
            MsvddRailMax = Caps.MsvddRailStockMaxMv;
            VoltageRailFloor = Caps.VoltageRailStockFloorMv;
            MsvddRailFloor = Caps.MsvddRailStockFloorMv;
            XbarOffset = 0;
        }
        OnPropertyChanged(nameof(BoostCeilingMv));   // the NVVDD ceiling feeds it
        RefreshAppliedSummary();
    }

    private void Reset()
    {
        try
        {
            _svc.ResetToDefaults();
            LoadIntoEditor(TuningProfile.Stock(Caps, Device.Name));
            XocEnabled = false;
            PendingChanges = false;
            Status = "Reset to driver defaults"; StatusIsError = false;
        }
        catch (Exception e) { Status = e.Message; StatusIsError = true; }
        RefreshAppliedSummary();
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
                                  nameof(TempLimit), nameof(VoltageBoost), nameof(VoltageOffset), nameof(TargetVoltage), nameof(VoltageRailMax), nameof(VoltageRailFloor), nameof(MsvddRailMax), nameof(MsvddRailFloor), nameof(XbarOffset), nameof(FixedFan) })
            RaiseVal(p);
        OnPropertyChanged(nameof(VoltageCapText));
       
    }
}
