using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using GpuTuner.Core.Models;

namespace GpuTuner.App.Controls;

/// <summary>
/// Voltage/frequency curve editor — the Afterburner Ctrl+F equivalent.
///
///   drag a point            move that point's frequency (voltage is fixed by the card)
///   Ctrl + drag             move the point AND flatten everything to its right (the undervolt)
///   Shift + drag            move every point from here rightwards by the same amount
///   double-click a point    return just that point to stock
///   right-click             return every point to stock
///
/// Voltages are fixed by the hardware, so points only move vertically.
/// </summary>
public sealed class VfCurveEditor : FrameworkElement
{
    private const double PadL = 46, PadR = 14, PadT = 14, PadB = 26;
    private const double HitRadius = 10;

    private List<VfCurveSample> _points = new();   // Index/VoltageMv/StockMhz fixed; LiveMhz is edited
    private int _dragIndex = -1;
    private int _dragStartMhz;
    private int[]? _dragStartAll;                   // snapshot of LiveMhz for shift-drag
    private (int vMin, int vMax, int fMin, int fMax)? _frozenAxes;  // held still while dragging

    public event EventHandler? CurveChanged;

    /// <summary>Points with their current (possibly edited) LiveMhz.</summary>
    public IReadOnlyList<VfCurveSample> Points => _points;

    /// <summary>True when any point differs from stock.</summary>
    public bool IsModified => _points.Any(p => p.LiveMhz != p.StockMhz);

    public void SetPoints(IEnumerable<VfCurveSample> pts)
    {
        _points = pts.OrderBy(p => p.VoltageMv).ToList();
        _dragIndex = -1;
        _dragStartAll = null;
        _frozenAxes = null;
        InvalidateVisual();
    }

    /// <summary>Return every point to its stock frequency.</summary>
    public void ResetToStock()
    {
        _points = _points.Select(p => p with { LiveMhz = p.StockMhz }).ToList();
        Raise();
    }

    /// <summary>
    /// Flatten every point at or above the given voltage to that point's current frequency.
    /// Returns false when no point sits at or below that voltage (nothing was changed).
    /// </summary>
    public bool FlattenFrom(int voltageMv)
    {
        int anchor = _points.FindLastIndex(p => p.VoltageMv <= voltageMv);
        if (anchor < 0) return false;
        FlattenFromIndex(anchor);
        Raise();
        return true;
    }

    private void FlattenFromIndex(int anchor)
    {
        int target = _points[anchor].LiveMhz;
        for (int i = anchor + 1; i < _points.Count; i++)
            _points[i] = _points[i] with { LiveMhz = target };
    }

    private double _liveVoltMv = double.NaN, _liveClockMhz = double.NaN;

    /// <summary>
    /// Active voltage-lock ceiling in mV, 0 when unlocked. The lock caps the operating point rather
    /// than rewriting the curve, so the stored points still slope upward past it — drawing the cap
    /// is the only way the plot can show what the card will actually do.
    /// </summary>
    private int _capMv;
    public void SetVoltageCap(int capMv) { _capMv = capMv; InvalidateVisual(); }
    public void SetLive(double voltageMv, double clockMhz)
    {
        _liveVoltMv = voltageMv; _liveClockMhz = clockMhz;
        InvalidateVisual();
    }

    private static readonly Brush GridBrush = Freeze(new SolidColorBrush(Color.FromArgb(34, 255, 255, 255)));
    private static readonly Brush LabelBrush = Freeze(new SolidColorBrush(Color.FromArgb(170, 255, 255, 255)));
    private static readonly Brush StockBrush = Freeze(new SolidColorBrush(Color.FromArgb(90, 200, 210, 225)));
    private static readonly Brush CurveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0x76, 0xB9, 0x00)));
    private static readonly Brush PointFill = Freeze(new SolidColorBrush(Color.FromRgb(0x14, 0x16, 0x1A)));
    private static readonly Brush LiveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0xE0, 0x70, 0x4B)));
    private static readonly Brush CapBrush = Freeze(new SolidColorBrush(Color.FromRgb(0xFF, 0x7A, 0x7A)));
    private static readonly Brush CapShade = Freeze(new SolidColorBrush(Color.FromArgb(26, 0xFF, 0x7A, 0x7A)));
    private static readonly Typeface Face = new("Segoe UI");
    private static Brush Freeze(Brush b) { b.Freeze(); return b; }

    public VfCurveEditor()
    {
        Focusable = true;
        Cursor = Cursors.Cross;
        ClipToBounds = true;
    }

    // ---- axes: voltage on X (from the points), frequency on Y (padded round numbers)
    /// <summary>
    /// Fixed axes matching Afterburner's curve editor: 700–1250 mV by 600–3400 MHz. A fixed frame
    /// keeps the curve's shape comparable between cards and runs, and stops the plot rescaling
    /// under the cursor mid-drag. Widened only if a card's curve genuinely falls outside it.
    /// </summary>
    private const int AxisVMin = 700, AxisVMax = 1250, AxisFMin = 600, AxisFMax = 3400;

    private (int vMin, int vMax, int fMin, int fMax) Bounds()
    {
        if (_frozenAxes != null) return _frozenAxes.Value;
        // Deliberately fixed, never fitted to the data. A card's curve starts around 450 mV, but
        // those are idle points nobody tunes; letting them widen the axis squashes the useful range
        // into the right-hand third. Afterburner frames 700-1250 mV for the same reason. Points
        // outside the frame are clipped, not drawn into the margins.
        return (AxisVMin, AxisVMax, AxisFMin, AxisFMax);
    }

    private double PlotW => Math.Max(1, ActualWidth - PadL - PadR);
    private double PlotH => Math.Max(1, ActualHeight - PadT - PadB);

    private Point ToScreen(double voltageMv, double freqMhz)
    {
        var (vMin, vMax, fMin, fMax) = Bounds();
        double x = PadL + (voltageMv - vMin) / (double)(vMax - vMin) * PlotW;
        double y = PadT + PlotH - (freqMhz - fMin) / (fMax - fMin) * PlotH;
        return new Point(x, y);
    }

    private int FreqFromY(double y)
    {
        var (_, _, fMin, fMax) = Bounds();
        double f = fMin + (PadT + PlotH - y) / PlotH * (fMax - fMin);
        int mhz = (int)Math.Round(f / 5.0) * 5;   // 5 MHz steps, like the driver's granularity
        return Math.Clamp(mhz, fMin, fMax);       // never let a drag off-screen ask for an absurd clock
    }

    protected override void OnRender(DrawingContext dc)
    {
        double w = ActualWidth, h = ActualHeight;
        if (w < 60 || h < 60) return;
        dc.DrawRectangle(Brushes.Transparent, null, new Rect(0, 0, w, h));   // hit-test surface

        double ppd = VisualTreeHelper.GetDpi(this).PixelsPerDip;
        var gridPen = new Pen(GridBrush, 1); gridPen.Freeze();

        if (_points.Count == 0)
        {
            var msg = new FormattedText("V/F curve unavailable on this card/driver.",
                CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 12, LabelBrush, ppd);
            dc.DrawText(msg, new Point(PadL, h / 2));
            return;
        }

        var (vMin, vMax, fMin, fMax) = Bounds();

        // horizontal grid — frequency
        int fStep = 200;   // matches Afterburner's 200 MHz gridlines
        for (int f = fMin; f <= fMax; f += fStep)
        {
            double y = Math.Round(ToScreen(vMin, f).Y) + 0.5;
            dc.DrawLine(gridPen, new Point(PadL, y), new Point(w - PadR, y));
            var ft = new FormattedText(f.ToString(), CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
            dc.DrawText(ft, new Point(PadL - ft.Width - 6, y - ft.Height / 2));
        }

        // vertical grid — voltage
        int vStep = 25;    // matches Afterburner's 25 mV gridlines
        // 25 mV gridlines are too dense to label individually on a narrow window, so label as many
        // as will fit without the text colliding.
        int labelEvery = Math.Max(1, (int)Math.Ceiling(30.0 / Math.Max(1, PlotW / ((vMax - vMin) / (double)vStep))));
        int vi = 0;
        for (int v = vMin; v <= vMax; v += vStep, vi++)
        {
            double x = Math.Round(ToScreen(v, fMin).X) + 0.5;
            dc.DrawLine(gridPen, new Point(x, PadT), new Point(x, h - PadB));
            if (vi % labelEvery != 0) continue;
            var ft = new FormattedText(v.ToString(), CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
            dc.DrawText(ft, new Point(x - ft.Width / 2, h - PadB + 5));
        }

        var unit = new FormattedText("mV", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
        dc.DrawText(unit, new Point(w - PadR + 2, h - PadB + 5));      // clear of the last tick label
        var yUnit = new FormattedText("MHz", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
        dc.DrawText(yUnit, new Point(4, PadT - 13));

        // Everything from here on is curve geometry: clip it to the plot so points outside the
        // frame (idle points below 700 mV, a live marker past the curve's top) can't spill into
        // the axis gutters.
        var plot = new Rect(PadL, PadT, Math.Max(0, w - PadL - PadR), Math.Max(0, h - PadT - PadB));
        dc.PushClip(new RectangleGeometry(plot));

        // stock curve (dim reference)
        var stockGeo = new StreamGeometry();
        using (var ctx = stockGeo.Open())
        {
            ctx.BeginFigure(ToScreen(_points[0].VoltageMv, _points[0].StockMhz), false, false);
            for (int i = 1; i < _points.Count; i++)
                ctx.LineTo(ToScreen(_points[i].VoltageMv, _points[i].StockMhz), true, false);
        }
        stockGeo.Freeze();
        var stockPen = new Pen(StockBrush, 1) { DashStyle = DashStyles.Dash }; stockPen.Freeze();
        dc.DrawGeometry(null, stockPen, stockGeo);

        // live (edited) curve
        var geo = new StreamGeometry();
        using (var ctx = geo.Open())
        {
            ctx.BeginFigure(ToScreen(_points[0].VoltageMv, _points[0].LiveMhz), false, false);
            for (int i = 1; i < _points.Count; i++)
                ctx.LineTo(ToScreen(_points[i].VoltageMv, _points[i].LiveMhz), true, false);
        }
        geo.Freeze();
        var pen = new Pen(CurveBrush, 2) { LineJoin = PenLineJoin.Round };
        dc.DrawGeometry(null, pen, geo);

        // points
        var ptPen = new Pen(CurveBrush, 2); ptPen.Freeze();
        for (int i = 0; i < _points.Count; i++)
        {
            var p = _points[i];
            var s = ToScreen(p.VoltageMv, p.LiveMhz);
            bool modified = p.LiveMhz != p.StockMhz;
            dc.DrawEllipse(i == _dragIndex || modified ? CurveBrush : PointFill, ptPen, s, 4.5, 4.5);
        }

        // readout for the dragged point
        if (_dragIndex >= 0 && _dragIndex < _points.Count)
        {
            var p = _points[_dragIndex];
            var s = ToScreen(p.VoltageMv, p.LiveMhz);
            int delta = p.LiveMhz - p.StockMhz;
            var ft = new FormattedText($"{p.VoltageMv} mV → {p.LiveMhz} MHz  ({delta:+#;-#;0})",
                CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11.5, LabelBrush, ppd);
            dc.DrawText(ft, new Point(Math.Min(s.X + 10, w - ft.Width - 4), Math.Max(PadT, s.Y - 20)));
        }

        // Voltage cap: everything to the right of it is unreachable, and the card behaves as if the
        // curve were flat from here on even though the stored points keep climbing.
        if (_capMv > 0 && _capMv >= vMin && _capMv <= vMax)
        {
            double capX = ToScreen(_capMv, fMin).X;
            dc.DrawRectangle(CapShade, null, new Rect(capX, PadT, Math.Max(0, w - PadR - capX), PlotH));

            var capPen = new Pen(CapBrush, 1.5); capPen.Freeze();
            dc.DrawLine(capPen, new Point(capX, PadT), new Point(capX, h - PadB));

            // effective (flat) behaviour above the cap
            var atCap = _points.Where(p => p.VoltageMv <= _capMv).OrderBy(p => p.VoltageMv).LastOrDefault();
            if (atCap.VoltageMv > 0)
            {
                var flatPen = new Pen(CapBrush, 2) { DashStyle = DashStyles.Dash }; flatPen.Freeze();
                double capY = ToScreen(_capMv, atCap.LiveMhz).Y;
                dc.DrawLine(flatPen, new Point(capX, capY), new Point(w - PadR, capY));
                var cft = new FormattedText($"capped {_capMv} mV → {atCap.LiveMhz} MHz",
                    CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11, CapBrush, ppd);
                dc.DrawText(cft, new Point(Math.Min(capX + 6, w - PadR - cft.Width - 2), Math.Max(PadT + 2, capY - 16)));
            }
        }

        // Live operating point. Only marked when it actually sits inside the frame — clamping it to
        // the edge would draw a dot on the curve at a voltage the card isn't running.
        if (!double.IsNaN(_liveVoltMv) && _liveVoltMv >= vMin && _liveVoltMv <= vMax)
        {
            double x = ToScreen(_liveVoltMv, fMin).X;
            var lp = new Pen(LiveBrush, 1) { DashStyle = DashStyles.Dash }; lp.Freeze();
            dc.DrawLine(lp, new Point(x, PadT), new Point(x, h - PadB));
            if (!double.IsNaN(_liveClockMhz) && _liveClockMhz >= fMin && _liveClockMhz <= fMax)
                dc.DrawEllipse(LiveBrush, null, ToScreen(_liveVoltMv, _liveClockMhz), 4, 4);
        }

        dc.Pop();   // release the plot clip
    }

    // ---- interaction
    private int HitTest(Point s)
    {
        int best = -1; double bestD = HitRadius;
        for (int i = 0; i < _points.Count; i++)
        {
            double d = (ToScreen(_points[i].VoltageMv, _points[i].LiveMhz) - s).Length;
            if (d <= bestD) { bestD = d; best = i; }
        }
        return best;
    }

    protected override void OnMouseLeftButtonDown(MouseButtonEventArgs e)
    {
        base.OnMouseLeftButtonDown(e);
        Focus();
        var pos = e.GetPosition(this);
        int hit = HitTest(pos);
        if (hit < 0) return;

        if (e.ClickCount == 2)
        {
            _points[hit] = _points[hit] with { LiveMhz = _points[hit].StockMhz };
            Raise();
            return;
        }

        _frozenAxes = Bounds();                    // snapshot while Bounds() still reflects the live points
        _dragIndex = hit;
        _dragStartMhz = _points[hit].LiveMhz;
        _dragStartAll = _points.Select(p => p.LiveMhz).ToArray();
        CaptureMouse();
        InvalidateVisual();
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        base.OnMouseMove(e);
        if (_dragIndex < 0 || !IsMouseCaptured || _dragStartAll == null) return;

        var pos = e.GetPosition(this);
        int newFreq = Math.Max(0, FreqFromY(pos.Y));

        bool shift = (Keyboard.Modifiers & ModifierKeys.Shift) != 0;
        bool ctrl = (Keyboard.Modifiers & ModifierKeys.Control) != 0;

        if (shift)
        {
            // move this point and everything to its right by the same amount
            int delta = newFreq - _dragStartMhz;
            for (int i = _dragIndex; i < _points.Count; i++)
                _points[i] = _points[i] with { LiveMhz = Math.Max(0, _dragStartAll[i] + delta) };
        }
        else
        {
            _points[_dragIndex] = _points[_dragIndex] with { LiveMhz = newFreq };
            // restore the untouched tail first so releasing Ctrl un-flattens
            for (int i = _dragIndex + 1; i < _points.Count; i++)
                _points[i] = _points[i] with { LiveMhz = _dragStartAll[i] };
            if (ctrl) FlattenFromIndex(_dragIndex);
        }

        Raise();
    }

    protected override void OnMouseLeftButtonUp(MouseButtonEventArgs e)
    {
        base.OnMouseLeftButtonUp(e);
        if (IsMouseCaptured) ReleaseMouseCapture();
        EndDrag();
    }

    /// <summary>Alt+Tab, a modal dialog or anything else that steals capture must also end the drag,
    /// otherwise the axes stay frozen and the readout stays on screen forever.</summary>
    protected override void OnLostMouseCapture(MouseEventArgs e)
    {
        base.OnLostMouseCapture(e);
        EndDrag();
    }

    private void EndDrag()
    {
        if (_dragIndex < 0 && _frozenAxes == null) return;
        _dragIndex = -1;
        _dragStartAll = null;
        _frozenAxes = null;
        InvalidateVisual();
    }

    protected override void OnMouseRightButtonDown(MouseButtonEventArgs e)
    {
        base.OnMouseRightButtonDown(e);
        if (_dragIndex >= 0) return;   // mid-drag the next MouseMove would restore the tail from the snapshot anyway
        ResetToStock();
    }

    private void Raise()
    {
        InvalidateVisual();
        CurveChanged?.Invoke(this, EventArgs.Empty);
    }
}
