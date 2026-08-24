using System.Windows;
using GpuTuner.App.Native;
using GpuTuner.App.ViewModels;

namespace GpuTuner.App;

/// <summary>
/// The gated levers: NVVDD and MSVDD rail ranges, the crossbar, SYS and video clock offsets, and the
/// clock range. Each has its own Enable/Disable pair. Shares the main window's view model, so the
/// values here are the same ones a saved profile carries - only each lever's own gate decides
/// whether it ever reaches the card.
/// </summary>
public partial class XocWindow : Window
{
    public XocWindow(MainViewModel vm)
    {
        DataContext = vm;
        InitializeComponent();
        WindowTheme.ApplyOnOpen(this);
    }
}
