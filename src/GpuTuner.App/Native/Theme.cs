using System;
using System.Collections.Generic;
using System.Windows;
using System.Windows.Media;

namespace GpuTuner.App.Native;

/// <summary>
/// Light and dark, switched live.
///
/// Shared Roch Viewer palette. Replacing the brush resources updates DynamicResource references
/// in every open window; custom-drawn controls follow the Changed event.
/// </summary>
public static class Theme
{
    /// <summary>Raised after the shared palette changes so custom-drawn controls can repaint.</summary>
    public static event EventHandler? Changed;

    /// <summary>Key, then the colour in light mode and in dark mode.</summary>
    private static readonly (string Key, string Light, string Dark)[] Palette =
    {
        ("BgBrush",          "#FFFFFFFF", "#FF000000"),
        ("PanelBrush",       "#FFFFFFFF", "#FF000000"),
        ("PanelAltBrush",    "#FFFFFFFF", "#FF000000"),
        ("BorderBrush",      "#FFD0D0D0", "#FF383838"),
        ("SectionBorderBrush", "#FFD0D0D0", "#FF383838"),
        ("TextBrush",        "#FF000000", "#FFFFFFFF"),
        ("MutedBrush",       "#FF62666D", "#FFA0A0A0"),
        ("HeaderBrush",      "#FFFFFFFF", "#FF000000"),
        ("HighlightBrush",   "#FFFFFFFF", "#FF000000"),
        ("BrandBrush",       "#FFD0343A", "#FFD0343A"),
        ("BrandHoverBrush",  "#FFE0383E", "#FFE0383E"),
        ("SelectedBrush",    "#FFD0343A", "#FFD0343A"),
        ("TitleHoverBrush",  "#FFE6E6E6", "#FF222222"),
        ("CloseHoverBrush",  "#FFC42B1C", "#FFC42B1C"),
        ("AccentBrush",      "#FFB91C1C", "#FFFF5A5F"),
        ("AccentDimBrush",   "#FFE7B9B9", "#FF6E1717"),
        ("WarnBrush",        "#FFB45309", "#FFFF7A7A"),
        ("DangerBrush",      "#FFDC2626", "#FFE53935"),
    };

    private static readonly List<Window> Windows = new();

    public static bool IsDark { get; private set; }

    /// <summary>Paint everything for the given mode. Safe to call before any window exists.</summary>
    public static void Apply(bool dark)
    {
        IsDark = dark;
        var res = Application.Current?.Resources;
        if (res == null) return;

        foreach (var (key, light, darkColour) in Palette)
        {
            // Replace the entry rather than recolour the brush. A ResourceDictionary freezes the
            // Freezables put into it, so the brush that comes back out cannot be assigned a new
            // colour - the first version of this tried and took the process down with it. Every
            // themed reference in the XAML is a DynamicResource, which is what makes replacing the
            // entry repaint each one, in every window already on screen.
            res[key] = new SolidColorBrush((Color)ColorConverter.ConvertFromString(dark ? darkColour : light)!);
        }

        // The title bar is drawn by Windows, not by WPF, so it has to be told separately - and every
        // window that is already open needs telling, not just the next one.
        foreach (var w in Windows.ToArray())
        {
            if (!w.IsLoaded && !w.IsVisible) continue;
            WindowTheme.SetDarkTitleBar(w, dark);
        }

        Changed?.Invoke(null, EventArgs.Empty);
    }

    /// <summary>
    /// Follow the theme for as long as this window lives. Windows are held only until they close,
    /// so a monitor opened and shut fifty times does not leave fifty dead references behind.
    /// </summary>
    public static void Register(Window window)
    {
        Windows.Add(window);
        window.Closed += (_, _) => Windows.Remove(window);
        WindowTheme.ApplyOnOpen(window, () => IsDark);
    }

    public static bool Toggle()
    {
        Apply(!IsDark);
        return IsDark;
    }
}
