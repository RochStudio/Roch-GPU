using System;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;

namespace GpuTuner.App.Native;

/// <summary>
/// Paints the window's title bar dark.
///
/// WPF does not own that strip — Windows draws it, and it follows the system light/dark setting,
/// not the app's. On a machine set to light mode a black app gets a white title bar, which is
/// exactly as jarring as it sounds. DWM exposes an opt-in for it.
/// </summary>
public static class WindowTheme
{
    // DWMWA_USE_IMMERSIVE_DARK_MODE. 20 since Windows 10 20H1; earlier builds used 19.
    private const int DarkModeAttribute = 20;
    private const int DarkModeAttributeLegacy = 19;

    [DllImport("dwmapi.dll", PreserveSig = true)]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

    /// <summary>Call once the window has an HWND — i.e. from SourceInitialized, not the constructor.</summary>
    public static void UseDarkTitleBar(Window window)
    {
        try
        {
            var hwnd = new WindowInteropHelper(window).Handle;
            if (hwnd == IntPtr.Zero) return;

            int on = 1;
            if (DwmSetWindowAttribute(hwnd, DarkModeAttribute, ref on, sizeof(int)) != 0)
                DwmSetWindowAttribute(hwnd, DarkModeAttributeLegacy, ref on, sizeof(int));
        }
        catch (DllNotFoundException) { /* not Windows 10+; the light title bar is survivable */ }
        catch (EntryPointNotFoundException) { }
    }

    /// <summary>Wire it up for a window that has not been shown yet.</summary>
    public static void ApplyOnOpen(Window window) =>
        window.SourceInitialized += (_, _) => UseDarkTitleBar(window);
}
