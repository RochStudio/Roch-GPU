using System;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;

namespace GpuTuner.App.Controls;

/// <summary>
/// Two thumbs on one track, for a value that is a range rather than a point — a voltage rail has a
/// floor and a ceiling, and showing them as two separate sliders hid the thing that matters, which
/// is the span between them.
///
/// WPF has no range slider, and this is deliberately not a templated control: it draws four
/// primitives and needs no styling surface, so a custom-drawn element is smaller than a ControlTemplate
/// would be and matches how the V/F curve editor is built.
/// </summary>
public sealed class RangeSlider : FrameworkElement
{
    public static readonly DependencyProperty MinimumProperty = Register(nameof(Minimum), 0.0);
    public static readonly DependencyProperty MaximumProperty = Register(nameof(Maximum), 100.0);
    public static readonly DependencyProperty LowerValueProperty = Register(nameof(LowerValue), 0.0);
    public static readonly DependencyProperty UpperValueProperty = Register(nameof(UpperValue), 100.0);
    public static readonly DependencyProperty StepProperty = Register(nameof(Step), 5.0);

    private static DependencyProperty Register(string name, double def) =>
        DependencyProperty.Register(name, typeof(double), typeof(RangeSlider),
            new FrameworkPropertyMetadata(def,
                FrameworkPropertyMetadataOptions.AffectsRender | FrameworkPropertyMetadataOptions.BindsTwoWayByDefault));

    public double Minimum { get => (double)GetValue(MinimumProperty); set => SetValue(MinimumProperty, value); }
    public double Maximum { get => (double)GetValue(MaximumProperty); set => SetValue(MaximumProperty, value); }
    public double LowerValue { get => (double)GetValue(LowerValueProperty); set => SetValue(LowerValueProperty, value); }
    public double UpperValue { get => (double)GetValue(UpperValueProperty); set => SetValue(UpperValueProperty, value); }
    public double Step { get => (double)GetValue(StepProperty); set => SetValue(StepProperty, value); }

    private static readonly Brush Track = Freeze(new SolidColorBrush(Color.FromRgb(0x3A, 0x3A, 0x42)));
    private static readonly Brush Span = Freeze(new SolidColorBrush(Color.FromRgb(0xD3, 0x2F, 0x2F)));
    private static readonly Brush Thumb = Freeze(new SolidColorBrush(Color.FromRgb(0xF3, 0xF3, 0xF5)));
    private static Brush Freeze(Brush b) { b.Freeze(); return b; }

    private const double ThumbW = 9, ThumbH = 15, TrackH = 3;

    /// <summary>Which thumb the pointer grabbed; null when not dragging.</summary>
    private bool? _draggingUpper;

    public RangeSlider()
    {
        Height = 17;
        Focusable = false;
        // Nothing templates this control, so the usual disabled-state trigger has nowhere to hang;
        // OnRender dims it instead, matching the 0.35 the nudge buttons use.
        IsEnabledChanged += (_, _) => InvalidateVisual();
    }

    private double Span01(double v)
    {
        double range = Maximum - Minimum;
        return range <= 0 ? 0 : Math.Clamp((v - Minimum) / range, 0, 1);
    }

    private double XFor(double v) => ThumbW / 2 + Span01(v) * Math.Max(1, ActualWidth - ThumbW);

    private double ValueAt(double x)
    {
        double t = Math.Clamp((x - ThumbW / 2) / Math.Max(1, ActualWidth - ThumbW), 0, 1);
        double v = Minimum + t * (Maximum - Minimum);
        if (Step > 0) v = Math.Round(v / Step) * Step;
        return Math.Clamp(v, Minimum, Maximum);
    }

    protected override void OnRender(DrawingContext dc)
    {
        double mid = ActualHeight / 2;
        double lo = XFor(LowerValue), hi = XFor(UpperValue);

        // A custom-drawn element is only hit-testable where it actually painted, which would leave
        // the grab area as the 3 px track line. Paint the whole height transparent first so the row
        // behaves like a normal slider.
        dc.DrawRectangle(Brushes.Transparent, null, new Rect(0, 0, Math.Max(0, ActualWidth), Math.Max(0, ActualHeight)));

        if (!IsEnabled) dc.PushOpacity(0.35);

        dc.DrawRoundedRectangle(Track, null,
            new Rect(0, mid - TrackH / 2, Math.Max(0, ActualWidth), TrackH), 1.5, 1.5);
        if (hi > lo)
            dc.DrawRoundedRectangle(Span, null, new Rect(lo, mid - TrackH / 2, hi - lo, TrackH), 1.5, 1.5);

        foreach (double x in new[] { lo, hi })
            dc.DrawRoundedRectangle(Thumb, null,
                new Rect(x - ThumbW / 2, mid - ThumbH / 2, ThumbW, ThumbH), 2, 2);

        if (!IsEnabled) dc.Pop();
    }

    protected override void OnMouseLeftButtonDown(MouseButtonEventArgs e)
    {
        double x = e.GetPosition(this).X;
        // Grab whichever thumb is nearer the click, so clicking anywhere on the track moves the
        // sensible end rather than always the same one.
        _draggingUpper = Math.Abs(x - XFor(UpperValue)) <= Math.Abs(x - XFor(LowerValue));
        CaptureMouse();
        Drag(x);
        e.Handled = true;
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        if (_draggingUpper != null && e.LeftButton == MouseButtonState.Pressed) Drag(e.GetPosition(this).X);
    }

    protected override void OnMouseLeftButtonUp(MouseButtonEventArgs e)
    {
        _draggingUpper = null;
        ReleaseMouseCapture();
    }

    private void Drag(double x)
    {
        double v = ValueAt(x);
        // The thumbs cannot cross: a floor above its ceiling is not a range the caller can act on.
        if (_draggingUpper == true) UpperValue = Math.Max(v, LowerValue);
        else LowerValue = Math.Min(v, UpperValue);
    }
}
