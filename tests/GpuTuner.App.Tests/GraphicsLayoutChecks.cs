using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Markup;
using System.Windows.Media;
using System.Xml.Linq;
using GpuTuner.App;
using GpuTuner.App.Native;
using GpuTuner.Core.Models;

internal static class GraphicsLayoutChecks
{
    internal static void Run(Action<string, bool> check)
    {
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            try
            {
                // Use the production resource definitions, without App's GPU/startup lifecycle.
                var app = new Application();
                XNamespace ns = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";
                var source = XDocument.Load(Path.Combine(AppContext.BaseDirectory, "AppResources.xaml"));
                var resources = new XElement(ns + "ResourceDictionary",
                    source.Root!.Attributes().Where(a => a.IsNamespaceDeclaration),
                    source.Root.Element(ns + "Application.Resources")!.Elements());
                app.Resources = (ResourceDictionary)XamlReader.Parse(resources.ToString().Replace(
                    "clr-namespace:GpuTuner.App.Controls", "clr-namespace:GpuTuner.App.Controls;assembly=RochGPU"));
                var info = new GpuGraphicsInfo { Gpu = "Intel Arc Pro B60", BoardManufacturer = "ASRock",
                    CodeName = "Battlemage BMG-G21 WKSTN", MemoryType = "GDDR6", DriverDate = "2026-09-17" };
                var window = new GraphicsWindow(info);
                var root = (FrameworkElement)window.Content;
                root.Measure(new Size(530, 340));
                root.Arrange(new Rect(0, 0, 530, 340));
                root.UpdateLayout();
                var items = Descendants(root).OfType<ItemsControl>().Single();
                var scroll = Descendants(root).OfType<ScrollViewer>().Single();
                check("Graphics binds every identity row", items.Items.Count == 17);
                check("Graphics displays bound board identity", Descendants(root).OfType<TextBlock>().Any(t => t.Text == "ASRock"));
                check("Graphics overflows into a vertical scrollbar", scroll.ScrollableHeight > 0);
                scroll.ScrollToEnd(); root.UpdateLayout();
                check("Graphics last rows remain reachable", scroll.VerticalOffset > 0 && Math.Abs(scroll.VerticalOffset - scroll.ScrollableHeight) < 1);
                Theme.Apply(true);
                var dark = ((SolidColorBrush)window.Background).Color;
                Theme.Apply(false);
                var light = ((SolidColorBrush)window.Background).Color;
                check("Graphics follows light and dark resources", dark != light);
                window.Close();
                app.Shutdown();
            }
            catch (Exception ex) { failure = ex; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        if (!thread.Join(15000)) throw new TimeoutException("Graphics layout checks did not finish.");
        if (failure != null) throw new InvalidOperationException("Graphics layout checks failed", failure);
    }

    private static IEnumerable<DependencyObject> Descendants(DependencyObject parent)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            var child = VisualTreeHelper.GetChild(parent, i);
            yield return child;
            foreach (var nested in Descendants(child)) yield return nested;
        }
    }
}
