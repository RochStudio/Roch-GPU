using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Markup;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using System.Xml.Linq;
using GpuTuner.App;
using GpuTuner.App.Native;
using GpuTuner.Core.Models;

/// <summary>
/// An opt-in, offscreen rendering path. It never creates the production App or a GPU backend.
/// These exports show only GraphicsWindow's WPF content; they are not desktop/runtime captures.
/// </summary>
internal static class GraphicsPreview
{
    private const double ContentWidth = 620;
    private const double ContentHeight = 600;
    private const double DisclosureHeight = 48;
    private static readonly int[] ExportDpis = [96, 144, 192];

    internal static int Run(string[] arguments)
    {
        if (arguments.Length != 2 || arguments[0] != "--graphics-preview")
        {
            Console.Error.WriteLine("Usage: GpuTuner.App.Tests --graphics-preview <workspace-output-directory>");
            return 2;
        }

        try
        {
            string outputDirectory = Path.GetFullPath(arguments[1]);
            // Refuse existing outputs rather than replacing screenshots or other user files.
            foreach (string path in OutputPaths(outputDirectory))
                if (File.Exists(path)) throw new IOException($"Preview output already exists: {path}");

            Exception? failure = null;
            var thread = new Thread(() =>
            {
                try { RenderAll(outputDirectory); }
                catch (Exception ex) { failure = ex; }
            }) { IsBackground = true, Name = "Graphics fixture preview" };
            thread.SetApartmentState(ApartmentState.STA);
            thread.Start();
            if (!thread.Join(15000)) throw new TimeoutException("Graphics fixture preview did not finish.");
            if (failure != null) throw new InvalidOperationException("Graphics fixture preview failed.", failure);

            Console.WriteLine($"Fixture preview: 6 offscreen PNGs in {outputDirectory}");
            Console.WriteLine("Synthetic data; content only; no production app launch or hardware interaction.");
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine(ex);
            return 1;
        }
    }

    private static void RenderAll(string outputDirectory)
    {
        // This is intentionally System.Windows.Application, not GpuTuner.App.App.
        var application = new System.Windows.Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        GraphicsWindow? window = null;
        try
        {
            // Identical resource-only extraction to GraphicsLayoutChecks: discard the Application
            // element/x:Class and parse just its resource definitions. No App startup is invoked.
            XNamespace ns = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";
            var source = XDocument.Load(Path.Combine(AppContext.BaseDirectory, "AppResources.xaml"));
            var resources = new XElement(ns + "ResourceDictionary",
                source.Root!.Attributes().Where(a => a.IsNamespaceDeclaration),
                source.Root.Element(ns + "Application.Resources")!.Elements());
            application.Resources = (ResourceDictionary)XamlReader.Parse(resources.ToString().Replace(
                "clr-namespace:GpuTuner.App.Controls", "clr-namespace:GpuTuner.App.Controls;assembly=RochGPU"));

            // Direct initializer only: FromDevice would query installed driver metadata.
            // Every value is illustrative, including the requested example card/board identity.
            var fixture = new GpuGraphicsInfo
            {
                Gpu = "Intel Arc Pro B60",
                BoardManufacturer = "ASRock",
                CodeName = "Battlemage BMG-G21 WKSTN (fixture)",
                Revision = "Example A0",
                Cores = "160",
                RopsTmus = "64 / 160",
                Technology = "5 nm",
                MemorySize = "24 GB",
                MemoryType = "GDDR6",
                MemoryVendor = "Example memory vendor",
                BusWidth = "192 bit",
                BusInterface = "4.0 x8",
                ResizableBar = "Enabled (fixture)",
                BiosVersion = "EXAMPLE-VBIOS",
                BusAddress = "EXAMPLE-PCI-ADDRESS",
                DriverVersion = "EXAMPLE-DRIVER",
                DriverDate = "2026-09-17 (fixture)",
            };

            Directory.CreateDirectory(outputDirectory);
            foreach (bool dark in new[] { false, true })
            {
                if (!ReferenceEquals(System.Windows.Application.Current, application) || !application.Dispatcher.CheckAccess())
                    throw new InvalidOperationException("Preview palette must belong to the plain Application on this STA thread.");
                Theme.Apply(dark);
                var expectedBackground = dark ? Colors.Black : Colors.White;
                var expectedBrand = Color.FromRgb(0xD0, 0x34, 0x3A);
                var expectedAccent = dark ? Color.FromRgb(0xFF, 0x5A, 0x5F) : Color.FromRgb(0xB9, 0x1C, 0x1C);
                var expectedText = dark ? Colors.White : Colors.Black;
                var expectedMuted = dark ? Color.FromRgb(0xA0, 0xA0, 0xA0) : Color.FromRgb(0x62, 0x66, 0x6D);
                AssertBrush("Application BgBrush", application.Resources["BgBrush"] as Brush, expectedBackground);
                AssertBrush("Application BrandBrush", application.Resources["BrandBrush"] as Brush, expectedBrand);
                AssertBrush("Application AccentBrush", application.Resources["AccentBrush"] as Brush, expectedAccent);
                AssertBrush("Application TextBrush", application.Resources["TextBrush"] as Brush, expectedText);
                AssertBrush("Application MutedBrush", application.Resources["MutedBrush"] as Brush, expectedMuted);

            // Construct a fresh view after applying each mode: existing XAML resource expressions
            // can retain their original window mentor when the content is detached.
            window = new GraphicsWindow(fixture);
            bool nativeWindowCreated = false;
            window.SourceInitialized += (_, _) => nativeWindowCreated = true;

            var content = (FrameworkElement)window.Content;
            // Detach the view from the never-shown window and reproduce its inherited text style.
            // The disclosure strip belongs only to the preview, not the production window.
            window.Content = null;
            content.DataContext = fixture;
            var preview = new Grid { UseLayoutRounding = window.UseLayoutRounding };
            // Preserve GraphicsWindow's safe local styles for deferred row-template creation.
            foreach (System.Collections.DictionaryEntry resource in window.Resources)
                preview.Resources.Add(resource.Key, resource.Value);
            preview.SetResourceReference(Panel.BackgroundProperty, "BgBrush");
            preview.SetResourceReference(TextElement.ForegroundProperty, "TextBrush");
            TextElement.SetFontFamily(preview, window.FontFamily);
            TextElement.SetFontSize(preview, window.FontSize);
            TextOptions.SetTextRenderingMode(preview, TextOptions.GetTextRenderingMode(window));
            TextOptions.SetTextFormattingMode(preview, TextOptions.GetTextFormattingMode(window));
            preview.RowDefinitions.Add(new RowDefinition { Height = new GridLength(DisclosureHeight) });
            preview.RowDefinitions.Add(new RowDefinition { Height = new GridLength(ContentHeight) });

            var disclosure = new TextBlock
            {
                Margin = new Thickness(14, 7, 14, 7),
                FontSize = 11,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
            };
            disclosure.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
            var disclosureBand = new Border { Child = disclosure };
            disclosureBand.SetResourceReference(Border.BackgroundProperty, "HeaderBrush");
            preview.Children.Add(disclosureBand);
            Grid.SetRow(content, 1);
            preview.Children.Add(content);

                // Application resource-change broadcasts target its windows. This detached root
                // is not a window, so mirror the actual production brushes into its local scope.
                // Updating this dictionary invalidates DynamicResource references in its subtree.
                foreach (System.Collections.DictionaryEntry resource in application.Resources)
                    if (resource.Value is Brush) preview.Resources[resource.Key] = resource.Value;
                Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
                foreach (int dpi in ExportDpis)
                {
                    disclosure.Text = "FIXTURE PREVIEW - Intel Arc Pro B60 / ASRock\n" +
                        $"Synthetic data - offscreen content only - {dpi} DPI export ({dpi * 100 / 96}%)";
                    var size = new Size(ContentWidth, ContentHeight + DisclosureHeight);
                    preview.Measure(size);
                    preview.Arrange(new Rect(new Point(), size));
                    preview.UpdateLayout();
                    Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
                    preview.UpdateLayout();

                    if (Descendants(content).OfType<ItemsControl>().Single().Items.Count != 17)
                        throw new InvalidOperationException("Fixture preview did not bind all 17 Graphics rows.");
                    AssertBrush("Resolved preview background", preview.Background, expectedBackground);
                    AssertTextBrushes(content, fixture, expectedText, expectedBrand, expectedAccent);
                    AssertBrush("Resolved fixture disclosure", disclosure.Foreground, expectedText);
                    if (nativeWindowCreated || window.IsVisible || window.IsLoaded)
                        throw new InvalidOperationException("Fixture preview must remain offscreen and unloaded.");

                    var bitmap = new RenderTargetBitmap(
                        (int)Math.Ceiling(size.Width * dpi / 96),
                        (int)Math.Ceiling(size.Height * dpi / 96), dpi, dpi, PixelFormats.Pbgra32);
                    bitmap.Render(preview);
                    var pixel = new byte[4];
                    bitmap.CopyPixels(new Int32Rect(0, (int)(DisclosureHeight * dpi / 96) + 1, 1, 1), pixel, 4, 0);
                    var renderedBackground = Color.FromArgb(pixel[3], pixel[2], pixel[1], pixel[0]);
                    if (renderedBackground != expectedBackground)
                        throw new InvalidOperationException($"Rendered background {renderedBackground} differs from {expectedBackground}.");
                    if (nativeWindowCreated || window.IsVisible || window.IsLoaded)
                        throw new InvalidOperationException("Rendering must not create or show a native window.");
                    var encoder = new PngBitmapEncoder();
                    encoder.Frames.Add(BitmapFrame.Create(bitmap));
                    string path = OutputPath(outputDirectory, dark, dpi);
                    using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
                    encoder.Save(stream);
                }
                Console.WriteLine($"{(dark ? "Dark" : "Light")}: BgBrush/resolved/rendered={expectedBackground}; Brand={expectedBrand}; headings/labels Text={expectedText}; values Accent={expectedAccent}; inactive Muted={expectedMuted}");
                window.Close();
                window = null;
            }
        }
        finally
        {
            // Closing an unshown GraphicsWindow removes its Theme registration. The plain WPF
            // Application has no production OnExit/fan-release or settings-persistence handler.
            window?.Close();
            application.Shutdown();
        }
    }

    private static IEnumerable<string> OutputPaths(string outputDirectory) =>
        from dark in new[] { false, true }
        from dpi in ExportDpis
        select OutputPath(outputDirectory, dark, dpi);

    private static string OutputPath(string outputDirectory, bool dark, int dpi) =>
        Path.Combine(outputDirectory, $"graphics-fixture-{(dark ? "dark" : "light")}-{dpi}dpi.png");

    private static void AssertBrush(string name, Brush? brush, Color expected)
    {
        if (brush is not SolidColorBrush solid || solid.Color != expected)
            throw new InvalidOperationException($"{name} must resolve to {expected}; actual={brush}.");
    }

    private static void AssertTextBrushes(FrameworkElement content, GpuGraphicsInfo fixture,
        Color expectedText, Color expectedBrand, Color expectedAccent)
    {
        var textBlocks = Descendants(content).OfType<TextBlock>().ToArray();
        void CheckText(string text, Color expected) =>
            AssertBrush($"Resolved text '{text}'", textBlocks.Single(t => t.Text == text).Foreground, expected);

        CheckText("Graphics", expectedText);
        CheckText("Static hardware and driver information", expectedText);
        CheckText("READ ONLY", expectedText);
        CheckText("GRAPHICS", expectedBrand);
        foreach (var row in fixture.Rows)
        {
            CheckText(row.Name, expectedText);
            CheckText(row.Value, expectedAccent);
        }
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
