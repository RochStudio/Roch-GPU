using System.Windows;
using GpuTuner.App.Native;
using GpuTuner.App.ViewModels;

namespace GpuTuner.App;

/// <summary>
/// The gated levers: NVVDD and MSVDD rail ranges and the crossbar clock offset. Shares the main
/// window's view model, so the values here are the same ones a saved profile carries - only the
/// Enable/Disable gate decides whether they ever reach the card.
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
