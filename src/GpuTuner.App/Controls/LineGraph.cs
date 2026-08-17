using System;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Media;

namespace GpuTuner.App.Controls;

/// <summary>
/// Minimal, dependency-free rolling line graph (OnRender-based, no NuGet charting lib needed).
/// Set Values (oldest→newest), Min/Max, Stroke; call InvalidateVisual() after updating.
/// </summary>
public sealed class LineGraph : FrameworkElement
{
    public static readonly DependencyProperty StrokeProperty =
        DependencyProperty.Register(nameof(Stroke), typeof(Brush), typeof(LineGraph),
            new FrameworkPropertyMetadata(Brushes.LimeGreen, FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty MinimumProperty =
        DependencyProperty.Register(nameof(Minimum), typeof(double), typeof(LineGraph),
            new FrameworkPropertyMetadata(0.0, FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty MaximumProperty =
        DependencyProperty.Register(nameof(Maximum), typeof(double), typeof(LineGraph),
            new FrameworkPropertyMetadata(100.0, FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty AutoScaleProperty =
        DependencyProperty.Register(nameof(AutoScale), typeof(bool), typeof(LineGraph),
            new FrameworkPropertyMetadata(false, FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty UnitProperty =
        DependencyProperty.Register(nameof(Unit), typeof(string), typeof(LineGraph),
            new FrameworkPropertyMetadata("", FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty MinSpanProperty =
        DependencyProperty.Register(nameof(MinSpan), typeof(double), typeof(LineGraph),
            new FrameworkPropertyMetadata(0.0, FrameworkPropertyMetadataOptions.AffectsRender));
    public static readonly DependencyProperty CapacityProperty =
        DependencyProperty.Register(nameof(Capacity), typeof(int), typeof(LineGraph),
            new FrameworkPropertyMetadata(120, FrameworkPropertyMetadataOptions.AffectsRender));

    public Brush Stroke { get => (Brush)GetValue(StrokeProperty); set => SetValue(StrokeProperty, value); }
    public double Minimum { get => (double)GetValue(MinimumProperty); set => SetValue(MinimumProperty, value); }
    public double Maximum { get => (double)GetValue(MaximumProperty); set => SetValue(MaximumProperty, value); }
    public bool AutoScale { get => (bool)GetValue(AutoScaleProperty); set => SetValue(AutoScaleProperty, value); }
    public string Unit { get => (string)GetValue(UnitProperty); set => SetValue(UnitProperty, value); }
    /// <summary>
    /// Smallest range the Y axis may show when AutoScale is on. Without this a dead-flat signal
    /// (idle clock sitting at 2835 MHz) gets scaled to a 2 MHz window and reads as violent noise.
    /// </summary>
    public double MinSpan { get => (double)GetValue(MinSpanProperty); set => SetValue(MinSpanProperty, value); }
    /// <summary>Number of samples the X axis represents (history length), so partial histories start from the left.</summary>
    public int Capacity { get => (int)GetValue(CapacityProperty); set => SetValue(CapacityProperty, value); }

    private double[] _values = Array.Empty<double>();
    public double[] Values
    {
        get => _values;
        set { _values = value ?? Array.Empty<double>(); InvalidateVisual(); }
    }

    private static readonly Brush GridBrush = new SolidColorBrush(Color.FromArgb(40, 255, 255, 255));
    private static readonly Brush LabelBrush = new SolidColorBrush(Color.FromArgb(160, 255, 255, 255));
    private static readonly Brush LabelBackdrop = new SolidColorBrush(Color.FromArgb(170, 20, 22, 26));
    private static readonly Typeface Face = new("Segoe UI");

    static LineGraph()
    {
        GridBrush.Freeze(); LabelBrush.Freeze(); LabelBackdrop.Freeze();
    }

    protected override void OnRender(DrawingContext dc)
    {
        double w = ActualWidth, h = ActualHeight;
        if (w < 4 || h < 4) return;

        // background handled by parent Border; draw grid
        var gridPen = new Pen(GridBrush, 1); gridPen.Freeze();
        for (int i = 1; i < 4; i++)
        {
            double y = Math.Round(h * i / 4) + 0.5;
            dc.DrawLine(gridPen, new Point(0, y), new Point(w, y));
        }

        var vals = _values.Where(v => !double.IsNaN(v)).ToArray();
        double min = Minimum, max = Maximum;
        if (AutoScale && vals.Length > 0)
        {
            min = vals.Min(); max = vals.Max();
            double span = max - min;
            if (span < MinSpan)
            {
                // Keep a sensible window around the data so a flat line renders flat.
                double mid = (max + min) / 2;
                min = mid - MinSpan / 2; max = mid + MinSpan / 2;
                if (min < 0 && vals.Min() >= 0) { max -= min; min = 0; }
            }
            else { double pad = span * 0.1; min -= pad; max += pad; }
        }
        if (max <= min) max = min + 1;

        // labels (max at top, min at bottom)
        DrawLabel(dc, $"{max:0}{Unit}", 3, 1);
        DrawLabel(dc, $"{min:0}{Unit}", 3, h - 14);

        if (_values.Length < 2) return;

        int cap = Math.Max(Capacity, _values.Length);
        double dx = w / (cap - 1);
        double xStart = w - dx * (_values.Length - 1);   // newest at right edge

        var geo = new StreamGeometry();
        using (var ctx = geo.Open())
        {
            bool started = false;
            for (int i = 0; i < _values.Length; i++)
            {
                double v = _values[i];
                if (double.IsNaN(v)) { started = false; continue; }
                double x = xStart + i * dx;
                double y = h - (Math.Clamp(v, min, max) - min) / (max - min) * h;
                if (!started) { ctx.BeginFigure(new Point(x, y), false, false); started = true; }
                else ctx.LineTo(new Point(x, y), true, false);
            }
        }
        geo.Freeze();

        // soft fill under the line
        var fillGeo = new StreamGeometry();
        using (var ctx = fillGeo.Open())
        {
            bool started = false; double lastX = 0;
            for (int i = 0; i < _values.Length; i++)
            {
                double v = _values[i]; if (double.IsNaN(v)) continue;
                double x = xStart + i * dx;
                double y = h - (Math.Clamp(v, min, max) - min) / (max - min) * h;
                if (!started) { ctx.BeginFigure(new Point(x, h), true, true); ctx.LineTo(new Point(x, y), false, false); started = true; }
                else ctx.LineTo(new Point(x, y), false, false);
                lastX = x;
            }
            if (started) ctx.LineTo(new Point(lastX, h), false, false);
        }
        fillGeo.Freeze();
        var fill = Stroke.Clone(); fill.Opacity = 0.12; fill.Freeze();
        dc.DrawGeometry(fill, null, fillGeo);

        // Freezing the pen would also freeze the (shared, unfrozen) resource brush passed in via Stroke.
        // Only freeze when the brush is already frozen (e.g. Brushes.LimeGreen default).
        var pen = new Pen(Stroke, 1.6) { LineJoin = PenLineJoin.Round };
        if (Stroke.IsFrozen) pen.Freeze();
        dc.DrawGeometry(null, pen, geo);
    }

    private void DrawLabel(DrawingContext dc, string text, double x, double y)
    {
        var ft = new FormattedText(text, CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Face, 10, LabelBrush,
            VisualTreeHelper.GetDpi(this).PixelsPerDip);
        dc.DrawRectangle(LabelBackdrop, null, new Rect(x - 2, y, ft.Width + 4, ft.Height));
        dc.DrawText(ft, new Point(x, y));
    }
}
