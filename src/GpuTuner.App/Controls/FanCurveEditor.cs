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
/// Interactive temperature→fan% curve editor.
///   drag a point        : move it (clamped, kept in temperature order)
///   double-click        : add a point
///   right-click a point : remove it (min 2 points)
/// Raises CurveChanged after every edit. Draws the live temperature/fan marker when set.
/// </summary>
public sealed class FanCurveEditor : FrameworkElement
{
    private const double PadL = 34, PadR = 12, PadT = 12, PadB = 22;
    private const double TMin = 0, TMax = 110, FMin = 0, FMax = 100;
    private const double HitRadius = 9;

    private List<FanPoint> _points = FanCurve.DefaultPoints();
    private int _dragIndex = -1;

    public event EventHandler? CurveChanged;

    public IReadOnlyList<FanPoint> Points => _points;

    public void SetPoints(IEnumerable<FanPoint> pts)
    {
        _points = pts.OrderBy(p => p.TemperatureC).ToList();
        if (_points.Count < 2) _points = FanCurve.DefaultPoints();
        InvalidateVisual();
    }

    private double _liveTemp = double.NaN, _liveFan = double.NaN;
    public void SetLive(double tempC, double fanPct)
    {
        _liveTemp = tempC; _liveFan = fanPct; InvalidateVisual();
    }

    public bool IsReadOnly { get; set; }

    private static readonly Brush GridBrush = Freeze(new SolidColorBrush(Color.FromArgb(38, 255, 255, 255)));
    private static readonly Brush LabelBrush = Freeze(new SolidColorBrush(Color.FromArgb(170, 255, 255, 255)));
    private static readonly Brush CurveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0x5D, 0xD0, 0xC7)));
    private static readonly Brush PointFill = Freeze(new SolidColorBrush(Color.FromRgb(0x14, 0x16, 0x1A)));
    private static readonly Brush LiveBrush = Freeze(new SolidColorBrush(Color.FromRgb(0xE0, 0x70, 0x4B)));
    private static readonly Typeface Face = new("Segoe UI");
    private static Brush Freeze(Brush b) { b.Freeze(); return b; }

    public FanCurveEditor()
    {
        Focusable = true;
        Cursor = Cursors.Cross;
    }

    // ---- coordinate mapping
    private double PlotW => Math.Max(1, ActualWidth - PadL - PadR);
    private double PlotH => Math.Max(1, ActualHeight - PadT - PadB);
    private Point ToScreen(FanPoint p) => new(
        PadL + (p.TemperatureC - TMin) / (TMax - TMin) * PlotW,
        PadT + PlotH - (p.FanPercent - FMin) / (FMax - FMin) * PlotH);
    private FanPoint ToData(Point s) => new(
        Math.Clamp(Math.Round(TMin + (s.X - PadL) / PlotW * (TMax - TMin)), TMin, TMax),
        Math.Clamp(Math.Round(FMax - (s.Y - PadT) / PlotH * (FMax - FMin)), FMin, FMax));

    protected override void OnRender(DrawingContext dc)
    {
        double w = ActualWidth, h = ActualHeight;
        if (w < 40 || h < 40) return;
        dc.DrawRectangle(Brushes.Transparent, null, new Rect(0, 0, w, h)); // hit-test area

        var gridPen = new Pen(GridBrush, 1); gridPen.Freeze();
        double ppd = VisualTreeHelper.GetDpi(this).PixelsPerDip;

        // horizontal grid (fan %)
        for (int f = 0; f <= 100; f += 20)
        {
            double y = Math.Round(ToScreen(new FanPoint(0, f)).Y) + 0.5;
            dc.DrawLine(gridPen, new Point(PadL, y), new Point(w - PadR, y));
            var ft = new FormattedText($"{f}%", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
            dc.DrawText(ft, new Point(PadL - ft.Width - 5, y - ft.Height / 2));
        }
        // vertical grid (temp)
        for (int t = 0; t <= 110; t += 10)
        {
            double x = Math.Round(ToScreen(new FanPoint(t, 0)).X) + 0.5;
            dc.DrawLine(gridPen, new Point(x, PadT), new Point(x, h - PadB));
            if (t % 20 == 0)
            {
                var ft = new FormattedText($"{t}°", CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush, ppd);
                dc.DrawText(ft, new Point(x - ft.Width / 2, h - PadB + 4));
            }
        }

        // curve
        var pts = _points.OrderBy(p => p.TemperatureC).ToList();
        var geo = new StreamGeometry();
        using (var ctx = geo.Open())
        {
            // extend flat to the left and right edges like the driver does
            var first = ToScreen(new FanPoint(TMin, pts[0].FanPercent));
            ctx.BeginFigure(first, false, false);
            foreach (var p in pts) ctx.LineTo(ToScreen(p), true, false);
            ctx.LineTo(ToScreen(new FanPoint(TMax, pts[^1].FanPercent)), true, false);
        }
        geo.Freeze();
        var pen = new Pen(CurveBrush, 2) { LineJoin = PenLineJoin.Round }; pen.Freeze();
        dc.DrawGeometry(null, pen, geo);

        // points
        var ptPen = new Pen(CurveBrush, 2); ptPen.Freeze();
        for (int i = 0; i < pts.Count; i++)
        {
            var s = ToScreen(pts[i]);
            dc.DrawEllipse(i == _dragIndex ? CurveBrush : PointFill, ptPen, s, 5, 5);
            if (i == _dragIndex)
            {
                var ft = new FormattedText($"{pts[i].TemperatureC:0}°C → {pts[i].FanPercent:0}%", CultureInfo.InvariantCulture,
                    FlowDirection.LeftToRight, Face, 11, LabelBrush, ppd);
                dc.DrawText(ft, new Point(Math.Min(s.X + 8, w - ft.Width - 4), Math.Max(PadT, s.Y - 18)));
            }
        }

        // live marker
        if (!double.IsNaN(_liveTemp))
        {
            var lp = new Pen(LiveBrush, 1) { DashStyle = DashStyles.Dash }; lp.Freeze();
            double x = ToScreen(new FanPoint(_liveTemp, 0)).X;
            dc.DrawLine(lp, new Point(x, PadT), new Point(x, h - PadB));
            if (!double.IsNaN(_liveFan))
                dc.DrawEllipse(LiveBrush, null, ToScreen(new FanPoint(_liveTemp, _liveFan)), 4, 4);
        }
    }

    // ---- interaction
    private int HitTest(Point s)
    {
        for (int i = 0; i < _points.Count; i++)
        {
            var p = ToScreen(_points[i]);
            if ((p - s).Length <= HitRadius) return i;
        }
        return -1;
    }

    protected override void OnMouseLeftButtonDown(MouseButtonEventArgs e)
    {
        base.OnMouseLeftButtonDown(e);
        if (IsReadOnly) return;
        Focus();
        var pos = e.GetPosition(this);
        if (e.ClickCount == 2)
        {
            var np = ToData(pos);
            _points.Add(np);
            _points = _points.OrderBy(p => p.TemperatureC).ToList();
            _dragIndex = _points.IndexOf(np);
            CaptureMouse();
            Raise();
            return;
        }
        int hit = HitTest(pos);
        if (hit >= 0) { _dragIndex = hit; CaptureMouse(); InvalidateVisual(); }
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        base.OnMouseMove(e);
        if (_dragIndex < 0 || !IsMouseCaptured) return;
        var d = ToData(e.GetPosition(this));
        // keep temperature strictly between neighbours so the curve stays a function
        double lo = _dragIndex > 0 ? _points[_dragIndex - 1].TemperatureC + 1 : TMin;
        double hi = _dragIndex < _points.Count - 1 ? _points[_dragIndex + 1].TemperatureC - 1 : TMax;
        d = new FanPoint(Math.Clamp(d.TemperatureC, lo, hi), d.FanPercent);
        _points[_dragIndex] = d;
        Raise();
    }

    protected override void OnMouseLeftButtonUp(MouseButtonEventArgs e)
    {
        base.OnMouseLeftButtonUp(e);
        if (IsMouseCaptured) ReleaseMouseCapture();
        _dragIndex = -1;
        InvalidateVisual();
    }

    protected override void OnMouseRightButtonDown(MouseButtonEventArgs e)
    {
        base.OnMouseRightButtonDown(e);
        if (IsReadOnly) return;
        int hit = HitTest(e.GetPosition(this));
        if (hit >= 0 && _points.Count > 2)
        {
            _points.RemoveAt(hit);
            Raise();
        }
    }

    private void Raise()
    {
        InvalidateVisual();
        CurveChanged?.Invoke(this, EventArgs.Empty);
    }
}
