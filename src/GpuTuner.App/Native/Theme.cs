using System;
using System.Collections.Generic;
using System.Windows;
using System.Windows.Media;

namespace GpuTuner.App.Native;

/// <summary>
/// Light and dark, switched live.
///
/// The colours are Roch Viewer's, which carries each one as a (light, dark) pair; this is the same
/// table read the same way. Its own switch is a single button showing the mode you are in, on the
/// reasoning that a Light/Dark pair spends half its width naming the mode you are not in — so this
/// is one button too.
///
/// Switching works by setting Color on the brush objects already in the resource dictionary rather
/// than swapping dictionaries. Every control in every open window holds a reference to those same
/// brushes, so assigning a colour repaints all of them at once, with no re-binding and no XAML
/// churn — the whole app, including windows that are already on screen. It only works while the
/// brushes are unfrozen, which is why none of them is declared with PresentationOptions:Freeze.
/// </summary>
public static class Theme
{
    /// <summary>Key, then the colour in light mode and in dark mode.</summary>
    private static readonly (string Key, string Light, string Dark)[] Palette =
    {
        ("BgBrush",          "#FFF1F5F9", "#FF101010"),
        ("PanelBrush",       "#FFFFFFFF", "#FF161616"),
        ("PanelAltBrush",    "#FFF8FAFC", "#FF1A1A1A"),
        ("BorderBrush",      "#FFCBD5E1", "#FF0A0A0A"),
        ("TextBrush",        "#FF0F172A", "#FFFFFFFF"),
        ("MutedBrush",       "#FF475569", "#FFB0B0B0"),
        ("HeaderBrush",      "#FFE2E8F0", "#FF1C1C1C"),
        ("HighlightBrush",   "#FFE8EEF5", "#FF171717"),
        // The brand red is saturated in light and lifted in dark: the dark one on a light ground is
        // too pale to read, and the light one on near-black is too dark. Roch Viewer splits it for
        // the same reason.
        ("BrandBrush",       "#FFB91C1C", "#FFFF4D4D"),
        ("BrandHoverBrush",  "#FFDC2626", "#FFFF8080"),
        ("TitleHoverBrush",  "#FFC5D2E0", "#FF343434"),
        ("CloseHoverBrush",  "#FFC42B1C", "#FFC42B1C"),
        ("AccentBrush",      "#FFB91C1C", "#FFD32F2F"),
        ("AccentDimBrush",   "#FFE7B9B9", "#FF6E1717"),
        ("WarnBrush",        "#FFB45309", "#FFFF7A7A"),
        ("DangerBrush",      "#FFDC2626", "#FFE53935"),
    };

    private static readonly List<Window> Windows = new();

    public static bool IsDark { get; private set; } = true;

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
