using System;
using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;
using GpuTuner.Core.Models;

namespace GpuTuner.App.Converters;

public sealed class BoolToVisibilityConverter : IValueConverter
{
    public bool Invert { get; set; }
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        bool b = value is bool v && v;
        if (Invert) b = !b;
        return b ? Visibility.Visible : Visibility.Collapsed;
    }
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) =>
        value is Visibility.Visible;
}

public sealed class BoolToBrushConverter : IValueConverter
{
    public Brush TrueBrush { get; set; } = Brushes.Red;
    public Brush FalseBrush { get; set; } = Brushes.Gray;
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture) =>
        value is bool b && b ? TrueBrush : FalseBrush;
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) => throw new NotSupportedException();
}

/// <summary>
/// Is one XOC lever armed? Keeps the view model to a single flags field instead of a boolean pair
/// per lever, and keeps the XAML naming the lever it belongs to rather than a property that only
/// differs by name. Invert feeds the Enable button, which is the one to offer while it is disarmed.
/// </summary>
public sealed class XocLeverConverter : IValueConverter
{
    public bool Invert { get; set; }
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        bool armed = value is XocLever set && parameter is string name
                     && Enum.TryParse<XocLever>(name, true, out var one) && set.Has(one);
        return Invert ? !armed : armed;
    }
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) =>
        throw new NotSupportedException();
}
