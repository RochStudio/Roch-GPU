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

/// <summary>
/// One of two themed brushes, looked up by resource key.
///
/// The keys are held as strings and resolved on every call rather than taking Brush values in XAML.
/// A converter is not a DependencyObject, so it cannot be given a DynamicResource - handed a
/// StaticResource it would capture whichever brush existed at load and keep showing that one colour
/// after a light/dark switch.
/// </summary>
public sealed class BoolToBrushConverter : IValueConverter
{
    public string TrueKey { get; set; } = "DangerBrush";
    public string FalseKey { get; set; } = "MutedBrush";

    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        string key = value is bool b && b ? TrueKey : FalseKey;
        return Application.Current?.TryFindResource(key) as Brush ?? Brushes.Gray;
    }
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

/// <summary>
/// What pressing a lever's gate button will do: "Disable" while it is armed, "Enable" while it is
/// not. The button names the action rather than the state, because the state is already told by the
/// slider beside it being live or dead.
/// </summary>
public sealed class XocGateLabelConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        bool armed = value is XocLever set && parameter is string name
                     && Enum.TryParse<XocLever>(name, true, out var one) && set.Has(one);
        return armed ? "Disable" : "Enable";
    }
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) =>
        throw new NotSupportedException();
}
