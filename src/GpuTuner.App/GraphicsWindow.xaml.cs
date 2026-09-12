using System.Windows;
using GpuTuner.App.Native;
using GpuTuner.Core.Models;

namespace GpuTuner.App;

/// <summary>Read-only static graphics-card details, matching Roch Viewer's Graphics section.</summary>
public partial class GraphicsWindow : Window
{
    public GraphicsWindow(GpuGraphicsInfo info)
    {
        DataContext = info;
        InitializeComponent();
        Theme.Register(this);
    }
}
