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
///   click a point           select it, then Up/Down nudge it by one 5 MHz driver step
///                           (Shift for 25 MHz, Ctrl to flatten above it, Left/Right to walk the curve)
///   double-click a point    return just that point to stock
///   right-click             return every point to stock
///
/// Voltages are fixed by the hardware, so points only move vertically.
/// </summary>
public sealed class VfCurveEditor : FrameworkElement
{
    // Bottom and left are wide enough for a tick label *and* an axis title under/beside it.
    private const double PadL = 58, PadR = 18, PadT = 18, PadB = 42;
    private const double HitRadius = 10;

    /// <summary>One press of Up/Down; matches the driver's 5 MHz granularity (see FreqFromY).</summary>
    private const int NudgeMhz = 5, NudgeCoarseMhz = 25;

    private List<VfCurveSample> _points = new();   // Index/VoltageMv/StockMhz fixed; LiveMhz is edited
    private int _dragIndex = -1;
    private int _selectedIndex = -1;               // survives the drag, so the arrow keys have a target
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
        RecomputeCapIndex();
        _dragIndex = -1;
        _selectedIndex = -1;      // indices refer to the old list; a reload invalidates the selection
        _dragStartAll = null;
        _frozenAxes = null;
        InvalidateVisual();
    }

    /// <summary>Return every point to its stock frequency.</summary>
    public void ResetToStock()
    {
        _points = _points.Select(p => p with { LiveMhz = p.StockMhz }).ToList();
        RecomputeCapIndex();
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
    public void SetVoltageCap(int capMv)
    {
        _capMv = capMv;
        RecomputeCapIndex();
        // A cap that has just swallowed the selected point would leave a white ring on a dot the
        // arrow keys can no longer move.
        if (_selectedIndex >= 0 && IsCapped(_selectedIndex)) _selectedIndex = -1;
        InvalidateVisual();
    }

    /// <summary>Frequency of the last point at or below the cap — the level the card flattens to.</summary>
    /// <summary>
    /// Index of the last point at or below the cap, or -1. Cached because it depends only on the cap
    /// and the points' voltages, neither of which move while the curve is being drawn or dragged —
    /// and it used to be re-found by a linear scan inside DisplayMhz, once per point per render.
    /// </summary>
    private int _capIndex = -1;

    private void RecomputeCapIndex() =>
        _capIndex = _capMv <= 0 ? -1 : _points.FindLastIndex(p => p.VoltageMv <= _capMv);

    private int CapFlatMhz() => _capIndex >= 0 ? _points[_capIndex].LiveMhz : 0;

    /// <summary>
    /// True for points the cap has made unreachable. Their stored frequency still exists and is still
    /// what gets written, but the card will never run there, so they are drawn flat and not edited.
    /// </summary>
    private bool IsCapped(int i) =>
        _capMv > 0 && i >= 0 && i < _points.Count && _points[i].VoltageMv > _capMv && CapFlatMhz() > 0;

    /// <summary>
    /// The frequency to *draw* point i at. Past an active cap the card behaves as though the curve
    /// were flat from the cap onward, which is what Afterburner plots and what the card actually
    /// does — so plot that, rather than stored points climbing to clocks nothing will ever reach.
    /// LiveMhz is left alone: it is what gets written, and it reappears the moment the cap lifts.
    /// </summary>
    private int DisplayMhz(int i) => IsCapped(i) ? CapFlatMhz() : _points[i].LiveMhz;

    /// <summary>
    /// Highest voltage the card is allowed to reach, once a boost is applied. The V/F table itself is
    /// fixed in hardware and stops at its last point (1090 mV on a 4070 Ti); a boost raises this
    /// ceiling above that without adding points, which otherwise just looks like the curve is missing
    /// data. 0 when it adds nothing beyond the table.
    /// </summary>
    private int _ceilingMv;
    public void SetVoltageCeiling(int mv) { _ceilingMv = mv; InvalidateVisual(); }
    public void SetLive(double voltageMv, double clockMhz)
    {
        _liveVoltMv = voltageMv; _liveClockMhz = clockMhz;
        InvalidateVisual();
    }

    private static readonly Brush GridBrush = Freeze(new SolidColorBrush(Color.FromArgb(34, 255, 255, 255)));
    private static readonly Brush LabelBrush = Freeze(new SolidColorBrush(Color.FromArgb(170, 255, 255, 255)));
    private static readonly Brush StockBrush = Freeze(new SolidColorBrush(Color.FromArgb(90, 200, 210, 225)));
    /// <summary>The edited curve: its line and its points are one object, so they are one colour.</summary>
    private static readonly Brush CurveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0xE0, 0x3C, 0x3C)));
    private static readonly Brush PointFill = Freeze(new SolidColorBrush(Color.FromRgb(0x14, 0x16, 0x1A)));
    private static readonly Brush SelectBrush = Freeze(new SolidColorBrush(Colors.White));
    private static readonly Brush LiveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0xE0, 0x70, 0x4B)));
    // The cap marker is white rather than red: red now belongs to the curve points, and two different
    // reds a shade apart read as one thing that has gone wrong somewhere.
    private static readonly Brush CeilingBrush = Freeze(new SolidColorBrush(Color.FromRgb(0x7F, 0xB0, 0xE8)));
    /// <summary>Wash over the stretch of curve the card cannot reach. Faint on purpose — it marks
    /// the region without hiding the points, which are still real table data.</summary>
    private static readonly Brush UnreachableBrush = Freeze(new SolidColorBrush(Color.FromArgb(0x18, 0x7F, 0xB0, 0xE8)));
    private static readonly Brush CapBrush = Freeze(new SolidColorBrush(Colors.White));
    private static readonly Brush CapShade = Freeze(new SolidColorBrush(Color.FromArgb(26, 255, 255, 255)));
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
    /// Fixed axes: 700–1200 mV by 600–3400 MHz. A fixed frame keeps the curve's shape comparable
    /// between cards and runs, and stops the plot rescaling under the cursor mid-drag. Widened only
    /// if a card's curve genuinely falls outside it.
    /// </summary>
    private const int AxisVMin = 700, AxisVMax = 1200, AxisFMin = 600, AxisFMax = 3400;

    private (int vMin, int vMax, int fMin, int fMax) Bounds()
    {
        if (_frozenAxes != null) return _frozenAxes.Value;
        // The low end is deliberately fixed rather than fitted: a card's curve starts around 450 mV,
        // and those idle points nobody tunes would squash the useful range into the right-hand third.
        // Afterburner frames 700-1250 mV for the same reason.
        //
        // The top ends are grown to fit, because clipping there hides real data — a 5070 Ti's curve
        // runs to 1240 mV and 3247 MHz, past the 1200/3400 frame, and the last points simply vanished
        // off the right-hand edge with nothing to say they existed.
        int vMax = AxisVMax, fMax = AxisFMax;
        if (_points.Count > 0)
        {
            int topMv = _points[^1].VoltageMv;
            if (topMv > vMax) vMax = (topMv + 24) / 25 * 25;          // next 25 mV tick
            int topMhz = 0;
            for (int i = 0; i < _points.Count; i++) topMhz = Math.Max(topMhz, DisplayMhz(i));
            if (topMhz > fMax) fMax = (topMhz + 199) / 200 * 200;     // next 200 MHz tick
        }
        return (AxisVMin, vMax, AxisFMin, fMax);
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

        // Axis titles, centred on their axis. The units used to be dropped into the margins beside
        // the outermost tick label, where "mV" landed on top of the last voltage number.
        var xTitle = new FormattedText("Voltage (mV)", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11, LabelBrush, ppd);
        dc.DrawText(xTitle, new Point(PadL + (PlotW - xTitle.Width) / 2, h - PadB + 22));

        var yTitle = new FormattedText("Core clock (MHz)", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11, LabelBrush, ppd);
        double yMid = PadT + PlotH / 2;
        dc.PushTransform(new RotateTransform(-90, 15, yMid));
        dc.DrawText(yTitle, new Point(15 - yTitle.Width / 2, yMid - yTitle.Height / 2));
        dc.Pop();

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
            ctx.BeginFigure(ToScreen(_points[0].VoltageMv, DisplayMhz(0)), false, false);
            for (int i = 1; i < _points.Count; i++)
                ctx.LineTo(ToScreen(_points[i].VoltageMv, DisplayMhz(i)), true, false);
        }
        geo.Freeze();
        var pen = new Pen(CurveBrush, 2) { LineJoin = PenLineJoin.Round };
        dc.DrawGeometry(null, pen, geo);

        // Past its last point the table has no more curve to follow, so the card holds that clock up
        // to whatever ceiling the boost allows. Afterburner runs its curve out to the edge of the axis
        // for the same reason. Drawn dashed and in the ceiling's colour because it is extrapolation,
        // not table data — there are no points out here to edit.
        int lastIdx = _points.Count - 1;
        int lastMv = _points[lastIdx].VoltageMv;
        if (_ceilingMv > lastMv && lastMv >= vMin)
        {
            double ey = ToScreen(lastMv, DisplayMhz(lastIdx)).Y;
            var extPen = new Pen(CeilingBrush, 2) { DashStyle = DashStyles.Dash }; extPen.Freeze();
            dc.DrawLine(extPen, new Point(ToScreen(lastMv, fMin).X, ey),
                                new Point(ToScreen(Math.Min(_ceilingMv, vMax), fMin).X, ey));
        }

        // points
        var ptPen = new Pen(CurveBrush, 2); ptPen.Freeze();
        var selPen = new Pen(SelectBrush, 2); selPen.Freeze();
        for (int i = 0; i < _points.Count; i++)
        {
            var p = _points[i];
            var s = ToScreen(p.VoltageMv, DisplayMhz(i));
            bool modified = !IsCapped(i) && p.LiveMhz != p.StockMhz;
            bool active = i == _dragIndex || i == _selectedIndex;
            // The selected point gets a white ring and a wider radius: it has to stay findable among
            // eighty-odd identical red dots while the arrow keys are walking it up and down.
            dc.DrawEllipse(modified || active ? CurveBrush : PointFill,
                           active ? selPen : ptPen, s, active ? 6 : 4.5, active ? 6 : 4.5);
        }

        // readout for whichever point is being worked on — dragged, or selected for the arrow keys
        int readout = _dragIndex >= 0 ? _dragIndex : _selectedIndex;
        if (readout >= 0 && readout < _points.Count)
        {
            var p = _points[readout];
            var s = ToScreen(p.VoltageMv, p.LiveMhz);
            int delta = p.LiveMhz - p.StockMhz;
            var ft = new FormattedText($"{p.VoltageMv} mV → {p.LiveMhz} MHz  ({delta:+#;-#;0})",
                CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11.5, LabelBrush, ppd);
            dc.DrawText(ft, new Point(Math.Min(s.X + 10, w - ft.Width - 4), Math.Max(PadT, s.Y - 20)));
        }

        // A cap is usually the reason the ceiling sits where it does, and then the two markers land on
        // the same millivolt: one line drawn over another, one label stacked on another, both saying
        // the same number. Let the cap marker carry the wording and skip the second one.
        bool ceilingIsTheCap = _capMv > 0 && _ceilingMv > 0 && Math.Abs(_ceilingMv - _capMv) <= 1;

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
                // Label the cap at the top of its line, not next to the flat segment. The selected
                // point's readout sits on the curve and now stays put after the mouse is released,
                // so anything drawn at curve height collides with it.
                var cft = new FormattedText(
                    $"capped {_capMv} mV → {atCap.LiveMhz} MHz" +
                    (ceilingIsTheCap ? " — points above are unreachable" : ""),
                    CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11, CapBrush, ppd);
                dc.DrawText(cft, new Point(Math.Min(capX + 6, w - PadR - cft.Width - 2), PadT + 4));
            }
        }

        // Where the card stops. Two different situations, and labelling one as the other would
        // mislead: a ceiling PAST the table's last point is boost headroom running beyond the curve,
        // while a ceiling INSIDE the curve means the table describes voltages this card never
        // reaches — every point above the line is drawn but unreachable, and cannot be tuned into.
        int lastPointMv = _points[^1].VoltageMv;
        if (_ceilingMv > 0 && !ceilingIsTheCap && _ceilingMv >= vMin && _ceilingMv <= vMax)
        {
            double cx = ToScreen(_ceilingMv, fMin).X;
            var cp = new Pen(CeilingBrush, 1.5) { DashStyle = DashStyles.Dot }; cp.Freeze();
            dc.DrawLine(cp, new Point(cx, PadT), new Point(cx, h - PadB));

            // Shade the unreachable stretch, faintly enough to read the curve through it.
            if (_ceilingMv < lastPointMv)
            {
                double rx = ToScreen(Math.Min(lastPointMv, vMax), fMin).X;
                if (rx > cx) dc.DrawRectangle(UnreachableBrush, null, new Rect(cx, PadT, rx - cx, h - PadB - PadT));
            }

            string label = _ceilingMv > lastPointMv
                ? $"boost ceiling {_ceilingMv} mV — table ends at {lastPointMv}"
                : $"card tops out at {_ceilingMv} mV — points above are unreachable";
            var t = new FormattedText(label,
                CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 11, CeilingBrush, ppd);
            // Label to the left of its line when there isn't room on the right.
            double tx = cx + 6 + t.Width < w - PadR ? cx + 6 : cx - 6 - t.Width;
            dc.DrawText(t, new Point(tx, PadT + 20));
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
            // Capped points are drawn flat and can't change what the card does, so they aren't grabbable.
            // Hit-test the drawn position, never the stored one, or the dots move out from under the cursor.
            if (IsCapped(i)) continue;
            double d = (ToScreen(_points[i].VoltageMv, DisplayMhz(i)) - s).Length;
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
        if (hit < 0)
        {
            // Clicking the empty plot drops the selection, so the arrow keys never move a point the
            // user has stopped thinking about.
            if (_selectedIndex >= 0) { _selectedIndex = -1; InvalidateVisual(); }
            return;
        }

        _selectedIndex = hit;

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

    /// <summary>
    /// Keyboard editing for the selected point. Up/Down nudge it by one 5 MHz driver step (Shift for
    /// 25, Ctrl also flattens everything above it, mirroring Ctrl+drag); Left/Right walk the
    /// selection along the curve. Handled is set on every arrow so WPF doesn't treat them as focus
    /// navigation and jump to the next control mid-edit.
    /// </summary>
    protected override void OnKeyDown(KeyEventArgs e)
    {
        base.OnKeyDown(e);
        if (_points.Count == 0) return;
        var (vMin, vMax, fMin, fMax) = Bounds();

        if (e.Key is Key.Left or Key.Right)
        {
            // Only walk points that are actually inside the frame — selecting an invisible idle
            // point below 700 mV would look like the keys had stopped working.
            int first = _points.FindIndex(p => p.VoltageMv >= vMin);
            int last = _points.FindLastIndex(p => p.VoltageMv <= vMax && (_capMv <= 0 || p.VoltageMv <= _capMv));
            if (first < 0 || last < first) return;

            int dir = e.Key == Key.Right ? 1 : -1;
            _selectedIndex = _selectedIndex < 0
                ? (dir > 0 ? first : last)
                : Math.Clamp(_selectedIndex + dir, first, last);
            e.Handled = true;
            InvalidateVisual();
            return;
        }

        if (e.Key is not (Key.Up or Key.Down) || _selectedIndex < 0 || _selectedIndex >= _points.Count) return;

        int step = (Keyboard.Modifiers & ModifierKeys.Shift) != 0 ? NudgeCoarseMhz : NudgeMhz;
        var pt = _points[_selectedIndex];
        int next = Math.Clamp(pt.LiveMhz + (e.Key == Key.Up ? step : -step), fMin, fMax);
        e.Handled = true;
        if (next == pt.LiveMhz) return;            // already against the top or bottom of the frame

        _points[_selectedIndex] = pt with { LiveMhz = next };
        if ((Keyboard.Modifiers & ModifierKeys.Control) != 0) FlattenFromIndex(_selectedIndex);
        Raise();
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
