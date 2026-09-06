using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using GpuTuner.App.Native;
using GpuTuner.App.ViewModels;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

namespace GpuTuner.App;

/// <summary>
/// Fan control in its own window: the mode, a duty per fan, and the curve editor.
///
/// The curve editor used to live in the hardware monitor, under a table of sensor readings it had
/// nothing to do with. Everything that decides what the fans do is here instead, and the main window
/// keeps the one-slider version for the common case.
///
/// Writes on Apply rather than on every drag: a fan slider dragged from 30 to 90 would otherwise send
/// sixty separate cooler writes on the way past.
/// </summary>
public partial class FanWindow : Window
{
    private readonly TuningService _svc;
    private readonly MainViewModel _vm;
    private readonly List<Slider> _sliders = new();
    private readonly List<TextBlock> _readouts = new();
    private bool _loading = true;

    public FanWindow(TuningService svc, MainViewModel vm)
    {
        _svc = svc;
        _vm = vm;
        DataContext = vm;
        InitializeComponent();
        Theme.Register(this);

        BuildFanRows();

        CurveEditor.SetPoints(_vm.EditorCurve.Points);
        HystBox.Text = _vm.EditorCurve.HysteresisC.ToString("0.#");
        StepBox.Text = _vm.EditorCurve.MinimumStepPercent.ToString("0.#");
        CurveEditor.CurveChanged += (_, _) =>
        {
            if (_loading) return;
            _vm.EditorCurve.Points = CurveEditor.Points.ToList();
            _vm.MarkCurveDirty();
        };
        CurveEditor.SelectionChanged += (_, _) => ShowSelectedPoint();
        ShowSelectedPoint();

        (_vm.FanModeIndex switch
        {
            1 => ModeFixed,
            2 => ModeCurve,
            _ => ModeAuto
        }).IsChecked = true;

        // The live readings drive the per-fan captions, so the sliders can be compared against what
        // the fans are actually doing rather than only against each other.
        _svc.TelemetryUpdated += OnTelemetry;
        Closed += (_, _) => _svc.TelemetryUpdated -= OnTelemetry;

        _loading = false;
        ShowPanelsForMode();
    }

    /// <summary>
    /// One row per cooler the card reports. Laid out here rather than in XAML because the count is a
    /// property of the card: a blower has one, this 5070 Ti has three, and a row for a fan that is
    /// not there would be a control that writes nowhere.
    /// </summary>
    private void BuildFanRows()
    {
        int count = Math.Max(1, _vm.Caps.FanCount);
        FixedNote.Text = count > 1
            ? $"This card reports {count} fans, each addressed separately by the driver."
            : "This card reports one fan.";

        for (int i = 0; i < count; i++)
        {
            var head = new Grid { Margin = new Thickness(0, 6, 0, 0) };
            head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var label = new TextBlock
            {
                Text = count > 1 ? $"Fan {i + 1}" : "Fan speed",
                Style = (Style)FindResource("RowLabel")
            };
            var readout = new TextBlock
            {
                Style = (Style)FindResource("Muted"),
                VerticalAlignment = VerticalAlignment.Center,
                Text = "—"
            };
            Grid.SetColumn(readout, 1);
            head.Children.Add(label);
            head.Children.Add(readout);
            _readouts.Add(readout);

            var slider = new Slider
            {
                Minimum = _vm.Caps.FanMinPercent,
                Maximum = _vm.Caps.FanMaxPercent <= 0 ? 100 : _vm.Caps.FanMaxPercent,
                Value = i < _vm.PerFanPercents.Length ? _vm.PerFanPercents[i] : _vm.FixedFan,
                Style = (Style)FindResource("Tight"),
                // Snap to the same 5% grid the main window's fan slider uses, so dragging lands on
                // the figures the arrows step through rather than between them.
                TickFrequency = Step,
                Tag = i
            };
            slider.ValueChanged += Slider_Changed;
            _sliders.Add(slider);

            // An arrow either side, as on every other slider in the app: a fan duty is worth dialling
            // in exactly, and dragging a 30-to-100 track to a particular 5 is fiddly.
            var track = new Grid { Margin = new Thickness(0, 1, 0, 0) };
            track.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            track.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            track.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var down = Nudge("◀", slider, -Step, new Thickness(0, 0, 4, 0));
            var up = Nudge("▶", slider, +Step, new Thickness(4, 0, 0, 0));
            Grid.SetColumn(slider, 1);
            Grid.SetColumn(up, 2);
            track.Children.Add(down);
            track.Children.Add(slider);
            track.Children.Add(up);

            FanRows.Children.Add(head);
            FanRows.Children.Add(track);
        }
    }

    /// <summary>
    /// Put the selected point into the two boxes. With nothing selected they are empty and disabled
    /// rather than showing a stale point: a box holding a number that edits nothing is worse than an
    /// empty one, because it invites a value that goes nowhere.
    /// </summary>
    private void ShowSelectedPoint()
    {
        var sel = CurveEditor.Selected;
        bool has = sel.HasValue;
        _loading = true;
        PointTemp.IsEnabled = PointFan.IsEnabled = has;
        PointTemp.Text = has ? sel!.Value.TemperatureC.ToString("0", CultureInfo.CurrentCulture) : "";
        PointFan.Text = has ? sel!.Value.FanPercent.ToString("0", CultureInfo.CurrentCulture) : "";
        PointLabel.Text = has ? $"Point {CurveEditor.SelectedIndex + 1}" : "Point";
        // Short enough to fit beside the boxes at this window width; the add and remove gestures are
        // explained in the paragraph above, where there is room for them.
        PointHint.Text = has ? "Enter to apply" : "Click a point to edit it";
        _loading = false;
    }

    private void PointBox_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Enter) return;
        CommitPoint();
        e.Handled = true;
    }

    private void PointBox_Commit(object sender, RoutedEventArgs e) => CommitPoint();

    /// <summary>
    /// Send whatever is typed to the editor, then read the point back into the boxes. The read-back
    /// matters: a temperature is held between its neighbours, so the number that lands is not always
    /// the number typed, and the box has to show which one won.
    /// </summary>
    private void CommitPoint()
    {
        if (_loading || CurveEditor.Selected is null) return;
        double? t = double.TryParse(PointTemp.Text, NumberStyles.Any, CultureInfo.CurrentCulture, out var tv) ? tv : null;
        double? f = double.TryParse(PointFan.Text, NumberStyles.Any, CultureInfo.CurrentCulture, out var fv) ? fv : null;
        CurveEditor.TryUpdateSelected(t, f);
        ShowSelectedPoint();
    }

    /// <summary>The step the arrows take, and the grid the slider snaps to. The same constant the
    /// main window nudges its fan slider by, so the two cannot drift apart.</summary>
    private const int Step = ClockStep.FanPercent;

    /// <summary>One nudge arrow, holdable, clamped to the slider's own range.</summary>
    private RepeatButton Nudge(string glyph, Slider target, int by, Thickness margin) 
    {
        var b = new RepeatButton
        {
            Content = glyph,
            Style = (Style)FindResource("Nudge"),
            Margin = margin
        };
        b.Click += (_, _) => target.Value = Math.Clamp(target.Value + by, target.Minimum, target.Maximum);
        return b;
    }

    /// <summary>Linked sliders move as one; unlinked, each is its own fan.</summary>
    private void Slider_Changed(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_loading || LinkFans.IsChecked != true) return;
        _loading = true;                       // stop the echo: setting the others re-enters here
        foreach (var s in _sliders) s.Value = e.NewValue;
        _loading = false;
    }

    private void LinkFans_Click(object sender, RoutedEventArgs e)
    {
        if (LinkFans.IsChecked != true || _sliders.Count == 0) return;
        _loading = true;
        foreach (var s in _sliders) s.Value = _sliders[0].Value;
        _loading = false;
    }

    private void Mode_Changed(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        ShowPanelsForMode();
    }

    private void ShowPanelsForMode()
    {
        bool fixedMode = ModeFixed.IsChecked == true;
        bool curve = ModeCurve.IsChecked == true;
        FixedPanel.Visibility = fixedMode ? Visibility.Visible : Visibility.Collapsed;
        CurvePanel.Visibility = curve ? Visibility.Visible : Visibility.Collapsed;
        ModeNote.Text = curve
            ? "The card has no curve of its own here, so this app steps the fans. It has to stay running for the curve to hold."
            : fixedMode
                ? "Held whatever the temperature does. The card's own thermal protection still applies."
                : "The driver decides. Nothing here is written until you pick Fixed or Curve.";
    }

    private void OnTelemetry(GpuTelemetry t) => Dispatcher.BeginInvoke(() =>
    {
        for (int i = 0; i < _readouts.Count; i++)
        {
            double duty = i < t.FanPercents.Length ? t.FanPercents[i] : t.FanPercent;
            double rpm = i < t.FanRpms.Length ? t.FanRpms[i] : t.FanRpm;
            _readouts[i].Text = rpm > 0
                ? $"{duty:0} %   ·   {rpm:0} rpm"
                : $"{duty:0} %";
        }
    });

    private void CurveParam_Changed(object sender, TextChangedEventArgs e)
    {
        if (_loading) return;
        if (double.TryParse(HystBox.Text, NumberStyles.Any, CultureInfo.CurrentCulture, out var h))
            _vm.EditorCurve.HysteresisC = Math.Clamp(h, 0, 20);
        if (double.TryParse(StepBox.Text, NumberStyles.Any, CultureInfo.CurrentCulture, out var s))
            _vm.EditorCurve.MinimumStepPercent = Math.Clamp(s, 0, 20);
        _vm.MarkCurveDirty();
    }

    private void DefaultCurve_Click(object sender, RoutedEventArgs e)
    {
        _vm.EditorCurve.Points = FanCurve.DefaultPoints();
        _loading = true;
        CurveEditor.SetPoints(_vm.EditorCurve.Points);
        _loading = false;
        _vm.MarkCurveDirty();
        ShowSelectedPoint();
    }

    /// <summary>
    /// Send it. Only the fan part of the profile is touched: someone in here is deciding about fans,
    /// not asking for a half-finished clock edit in the main window to be applied as well.
    /// </summary>
    private void Apply_Click(object sender, RoutedEventArgs e)
    {
        var mode = ModeCurve.IsChecked == true ? FanMode.Curve
                 : ModeFixed.IsChecked == true ? FanMode.Fixed
                 : FanMode.Auto;
        int[] duties = LinkFans.IsChecked == true || _sliders.Count < 2
            ? Array.Empty<int>()
            : _sliders.Select(s => (int)Math.Round(s.Value)).ToArray();

        var errs = _svc.SetFans(mode, (int)Math.Round(_sliders.Count > 0 ? _sliders[0].Value : 50),
                                duties, _vm.EditorCurve);
        bool ok = errs.Count == 0 || TuningService.OnlyNotes(errs);
        Status.Text = errs.Count > 0 ? string.Join("  |  ", errs) : Describe(mode, duties);
        Status.Foreground = (System.Windows.Media.Brush)FindResource(ok ? "MutedBrush" : "DangerBrush");

        // Keep the main window's own fan controls telling the same story.
        _vm.SyncFansFromService(mode, duties);
    }

    private string Describe(FanMode mode, int[] duties) => mode switch
    {
        FanMode.Auto => "Fans handed back to the driver.",
        FanMode.Curve => "Curve running. It stops if this app closes.",
        _ => duties.Length > 1
            ? "Set: " + string.Join(", ", duties.Select((d, i) => $"fan {i + 1} {d}%"))
            : $"All fans at {(_sliders.Count > 0 ? (int)Math.Round(_sliders[0].Value) : 0)}%."
    };
}
