using GpuTuner.App.Native;
using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Media;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;

namespace GpuTuner.App;

/// <summary>
/// Graphical V/F curve editor window. Reads the curve from the backend, lets the user drag points,
/// and writes explicit per-point targets back. Everything is volatile — "Reset to stock" or a reboot
/// undoes it.
/// </summary>
public partial class CurveWindow : Window
{
    private readonly TuningService _svc;

    public CurveWindow(TuningService svc)
    {
        _svc = svc;
        InitializeComponent();
        WindowTheme.ApplyOnOpen(this);

        Editor.CurveChanged += (_, _) => UpdateStatus();
        Loaded += (_, _) => LoadCurve();

        _svc.TelemetryUpdated += OnTelemetry;
        Closed += (_, _) => _svc.TelemetryUpdated -= OnTelemetry;
    }

    // Raised on the polling thread. BeginInvoke throws once the dispatcher is shutting down, and the
    // Closed unsubscribe can lose the race with a sample already in flight, so check first.
    private void OnTelemetry(GpuTelemetry t)
    {
        if (Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) return;
        Dispatcher.BeginInvoke(() =>
        {
            Editor.SetLive(t.VoltageMv, t.CoreClockMhz);
            Editor.SetVoltageCap(ReadCapMv());
            Editor.SetVoltageCeiling(ReadCeilingMv());
        });
    }

    private void LoadCurve()
    {
        try
        {
            var pts = _svc.ReadVfCurve();
            Editor.SetPoints(pts);
            Editor.SetVoltageCap(ReadCapMv());
            int ceiling = ReadCeilingMv();
            Editor.SetVoltageCeiling(ceiling);
            if (pts.Count == 0)
            {
                Say("This card/driver did not return a V/F curve — nothing to edit.", true);
                return;
            }

            int top = pts.Max(p => p.VoltageMv);
            // The table's top is a hardware fact, so say where it is when a boost has pushed the
            // ceiling past it — otherwise the curve just looks like it stops short for no reason.
            string boost = ceiling > top
                ? $" The table stops at {top} mV; the boost lets the card run to {ceiling} mV, but adds no points."
                : "";
            Say($"{pts.Count} curve points loaded ({pts.Min(p => p.VoltageMv)}–{top} mV).{boost} " +
                "Drag a point, or click one and use ↑/↓ (Shift = 25 MHz, ←/→ to step along). Then Apply to GPU.");
        }
        catch (Exception ex) { Say("Could not read the curve: " + ex.Message, true); }
    }

    private void UpdateStatus()
    {
        var pts = Editor.Points;
        if (pts.Count == 0) return;
        int changed = pts.Count(p => p.LiveMhz != p.StockMhz);
        if (changed == 0) { Say("Curve matches stock."); return; }
        int maxUp = pts.Max(p => p.LiveMhz - p.StockMhz);
        int maxDown = pts.Min(p => p.LiveMhz - p.StockMhz);
        Say($"{changed} of {pts.Count} points edited (max {maxUp:+#;-#;0} / {maxDown:+#;-#;0} MHz). Not applied yet.");
    }

    /// <summary>
    /// Flatten = cap the voltage. The delta table only owns the lower curve points (the driver
    /// derives the top ~23 from them), so a per-point flatten physically cannot reach a cap up at
    /// 1000 mV. The voltage lock expresses the same intent and applies immediately.
    /// </summary>
    private void Flatten_Click(object sender, RoutedEventArgs e)
    {
        if (!int.TryParse(FlattenBox.Text.Trim(), out int mv)) { Say("Enter a voltage in mV.", true); return; }

        // This box is free text, so clamp it to the same window the main slider offers. Without this
        // a typo goes straight to the driver, and a big enough number overflows the microvolt
        // conversion into something arbitrary.
        var caps = _svc.Capabilities;
        int lo = caps.MinVoltageMv > 0 ? caps.MinVoltageMv : 600;
        int hi = caps.MaxVoltageMv > 0 ? caps.MaxVoltageMv : 1200;
        if (mv < lo || mv > hi) { Say($"Voltage must be between {lo} and {hi} mV on this card.", true); return; }

        try
        {
            if (_svc.Backend is GpuTuner.Core.Backends.Nvidia.NvApiBackend nv)
            {
                nv.SetVoltageLock(_svc.GpuIndex, mv);
                Editor.SetVoltageCap(mv);
                Editor.FlattenFrom(mv);            // mirror it in the plot's own points too
                Say($"Voltage capped at {mv} mV — applied to the GPU now. Check the live voltage.");
                return;
            }
            if (!Editor.FlattenFrom(mv)) { Say($"No curve point at or below {mv} mV — nothing to flatten.", true); return; }
            Say($"Flattened everything above {mv} mV. Press Apply to GPU to commit.");
        }
        catch (Exception ex) { Say("Could not set the voltage cap: " + ex.Message, true); }
    }

    private int ReadCapMv()
    {
        try
        {
            // Ask through the interface rather than casting to the NVIDIA backend: ReadVoltageLockMv
            // already returns -1 for "can't report", which is the same answer the cast was producing
            // for everything else, and going through it lets the mock draw a cap too.
            int mv = _svc.Backend.ReadVoltageLockMv(_svc.GpuIndex);
            return mv > 0 ? mv : 0;
        }
        catch { return 0; }
    }

    /// <summary>
    /// The highest voltage the card may reach with the applied boost, which is not the same as the
    /// top of the V/F table — the table is fixed in hardware and a boost adds no points to it.
    /// </summary>
    private int ReadCeilingMv()
    {
        try
        {
            var st = _svc.Backend.ReadTuningState(_svc.GpuIndex);
            var caps = _svc.Capabilities;
            return VoltagePlan.ToTargetMv(st.VoltageBoostPercent, st.VoltageOffsetMv,
                                          caps.StockMaxVoltageMv, caps.MaxVoltageMv);
        }
        catch { return 0; }
    }

    private void Reload_Click(object sender, RoutedEventArgs e) => LoadCurve();

    private void ResetStock_Click(object sender, RoutedEventArgs e)
    {
        Editor.ResetToStock();
        try
        {
            _svc.SetVfCurveTargets(Array.Empty<VfCurveSample>());
            Say("Curve cleared on the GPU — back to stock.");
        }
        catch (Exception ex) { Say("Reset failed: " + ex.Message, true); }
    }

    private void ApplyCurve_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            // Only send the points that actually differ; the backend returns the rest to stock.
            var targets = Editor.Points.Where(p => p.LiveMhz != p.StockMhz).ToList();
            _svc.SetVfCurveTargets(targets);

            // Read back so the user sees what the driver really accepted (it snaps to its own grid).
            var after = _svc.ReadVfCurve();
            if (after.Count > 0) Editor.SetPoints(after);

            int applied = after.Count(p => p.LiveMhz != p.StockMhz);
            Say(targets.Count == 0
                ? "Curve cleared — back to stock."
                : after.Count == 0
                    ? "Applied, but the driver did not return the curve for read-back — cannot confirm what stuck."
                    : $"Applied. Driver reports {applied} point(s) changed. Graph now shows what the card actually took.");
        }
        catch (Exception ex) { Say("Apply failed: " + ex.Message, true); }
    }

    private void Say(string msg, bool error = false)
    {
        StatusText.Text = msg;
        StatusText.Foreground = error
            ? (Brush)FindResource("DangerBrush")
            : (Brush)FindResource("MutedBrush");
    }
}
