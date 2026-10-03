using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Shapes;

namespace GpuTuner.App.Controls;

/// <summary>The shared filled crescent for the light theme's switch-to-dark action.</summary>
public static class ThemeGlyph
{
    public const string SolidMoonData = "M14.125,1.323927 A8,8 0 1 0 14.125,16.676073 A8,8 0 0 1 14.125,1.323927 Z";

    public static Canvas CreateDarkTargetMoon()
    {
        var canvas = new Canvas { Width = 18, Height = 18 };
        canvas.Children.Add(new Path { Data = Geometry.Parse(SolidMoonData), Fill = Brushes.Black });
        return canvas;
    }
}
