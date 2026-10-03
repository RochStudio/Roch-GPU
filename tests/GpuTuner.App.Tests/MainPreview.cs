using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Markup;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using System.Xml.Linq;

/// <summary>
/// Renders source MainWindow.xaml content with inert fixture data. No Window, production view
/// model, production Application, native theme helper or hardware service is constructed.
/// </summary>
internal static class MainPreview
{
    private static readonly XNamespace Ui = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";
    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";
    private const string ConverterNamespace = "clr-namespace:GpuTuner.App.Converters";
    private const string SolidMoonData = "M14.125,1.323927 A8,8 0 1 0 14.125,16.676073 A8,8 0 0 1 14.125,1.323927 Z";
    private static readonly int[] Dpis = [96, 144, 192];
    private static readonly HashSet<string> EventAttributes = new(StringComparer.Ordinal)
    {
        "Click", "MouseLeftButtonDown", "MouseLeftButtonUp", "MouseRightButtonUp",
        "Loaded", "Unloaded", "SourceInitialized", "Closing", "Closed", "Initialized",
        "KeyDown", "PreviewKeyDown", "SelectionChanged", "ValueChanged", "Checked", "Unchecked",
        "StateChanged", "ContentRendered",
    };
    private static readonly HashSet<string> AllowedMainNodes = new(StringComparer.Ordinal)
    {
        "Window", "Window.Resources", "Border", "Button", "Canvas", "Path", "CheckBox", "ColumnDefinition", "ComboBox",
        "ContentPresenter", "ControlTemplate", "ControlTemplate.Triggers", "DataTemplate", "DataTrigger",
        "DockPanel", "Grid", "Grid.ColumnDefinitions", "Grid.RowDefinitions", "Image", "ItemsControl",
        "ItemsControl.ItemsPanel", "ItemsControl.ItemTemplate", "ItemsPanelTemplate", "RepeatButton",
        "RepeatButton.Template", "RowDefinition", "Run", "Setter", "Setter.Value", "Slider", "StackPanel",
        "Style", "Style.Triggers", "TextBlock", "TextBlock.Style", "TextBox", "Thumb", "Thumb.Template",
        "ToggleButton", "Track", "Track.DecreaseRepeatButton", "Track.IncreaseRepeatButton", "Track.Thumb", "Trigger",
    };

    internal static int Run(string[] arguments)
    {
        if (arguments.Length != 2 || arguments[0] != "--main-preview")
        {
            Console.Error.WriteLine("Usage: GpuTuner.App.Tests --main-preview <workspace-output-directory>");
            return 2;
        }
        try
        {
            string directory = Path.GetFullPath(arguments[1]);
            foreach (string path in OutputPaths(directory))
                if (File.Exists(path)) throw new IOException($"Preview output already exists: {path}");
            Exception? failure = null;
            var thread = new Thread(() =>
            {
                try { RenderAll(directory); }
                catch (Exception ex) { failure = ex; }
            }) { IsBackground = true, Name = "Main XAML fixture preview" };
            thread.SetApartmentState(ApartmentState.STA);
            thread.Start();
            if (!thread.Join(20000)) throw new TimeoutException("Main XAML fixture preview did not finish.");
            if (failure != null) throw new InvalidOperationException("Main XAML fixture preview failed.", failure);
            Console.WriteLine($"Main fixture preview: 6 offscreen PNGs and provenance in {directory}");
            return 0;
        }
        catch (Exception ex) { Console.Error.WriteLine(ex); return 1; }
    }

    private static void RenderAll(string directory)
    {
        var application = new System.Windows.Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        try
        {
            string mainPath = Path.Combine(AppContext.BaseDirectory, "MainWindowSource.xaml");
            string resourcesPath = Path.Combine(AppContext.BaseDirectory, "AppResources.xaml");
            string palettePath = Path.Combine(AppContext.BaseDirectory, "ThemeSource.txt");
            var source = XDocument.Load(mainPath);
            var appSource = XDocument.Load(resourcesPath);
            var provenance = new List<string>
            {
                "FIXTURE PREVIEW: source WPF content, not a production app or live GPU capture.",
                "No Window constructed. Hardware actions disabled. Checkbox/combo state is in-memory only.",
                "DPI values are bitmap export resolutions; native/interactive Windows DPI behavior is not tested.",
                $"UTC: {DateTime.UtcNow:O}",
                $"MainWindowSource.xaml SHA256: {Hash(mainPath)}",
                $"AppResources.xaml SHA256: {Hash(resourcesPath)}",
                $"ThemeSource.txt SHA256: {Hash(palettePath)}",
                "THEME GEOMETRY: production source Canvas/Path retained in light mode; 18x18 solid black crescent, no stroke.",
                "THEME GEOMETRY DATA: " + SolidMoonData,
                "DARK THEME TARGET: original white Segoe UI Symbol sun U+2600, 18px.",
            };
            if (source.Root?.Name != Ui + "Window" || (string?)source.Root.Attribute(X + "Class") != "GpuTuner.App.MainWindow")
                throw new InvalidOperationException("Expected the production MainWindow source.");
            ValidateMainNodes(source.Root);
            ValidateHoverSources(source.Root, provenance);
            foreach (var attribute in source.Root.DescendantsAndSelf().Attributes().ToArray())
            {
                if (attribute.Name == X + "Class" || (attribute.Name.Namespace == XNamespace.None && EventAttributes.Contains(attribute.Name.LocalName)))
                {
                    provenance.Add($"STRIPPED {attribute.Parent!.Name.LocalName}.{attribute.Name}={attribute.Value}");
                    attribute.Remove();
                }
            }
            foreach (var image in source.Root.Descendants(Ui + "Image"))
            {
                var attribute = image.Attribute("Source") ?? throw new InvalidOperationException("Image source is required.");
                if (attribute.Value != "pack://application:,,,/icon.png")
                    throw new InvalidOperationException($"Unreviewed image URI: {attribute.Value}");
                attribute.Value = "pack://application:,,,/RochGPU;component/icon.png";
                provenance.Add("LOCAL IMAGE URI: pack://application:,,,/icon.png -> pack://application:,,,/RochGPU;component/icon.png");
            }
            provenance.Add("DISCARDED Window container metadata: " + string.Join(", ", source.Root.Attributes().Where(a => !a.IsNamespaceDeclaration).Select(a => $"{a.Name}={a.Value}")));

            var paletteBlock = Regex.Match(File.ReadAllText(palettePath), @"Palette\s*=\s*\{(?<pairs>[\s\S]*?)\};");
            var palette = Regex.Matches(paletteBlock.Groups["pairs"].Value,
                """\("(?<key>[^\"]+)",\s*"(?<light>#[0-9A-Fa-f]{8})",\s*"(?<dark>#[0-9A-Fa-f]{8})"\)""")
                .Cast<Match>().ToDictionary(m => m.Groups["key"].Value,
                    m => (Light: m.Groups["light"].Value, Dark: m.Groups["dark"].Value));
            if (palette.Count < 18) throw new InvalidOperationException("Expected the production palette literal table.");

            Directory.CreateDirectory(directory);
            foreach (bool dark in new[] { false, true })
            {
                AssertNoWindows(application);
                // Parse resources only. Palette source is text, never production Theme code.
                var resources = new XElement(Ui + "ResourceDictionary",
                    appSource.Root!.Attributes().Where(a => a.IsNamespaceDeclaration),
                    appSource.Root.Element(Ui + "Application.Resources")!.Elements());
                application.Resources = (ResourceDictionary)XamlReader.Parse(resources.ToString().Replace(
                    "clr-namespace:GpuTuner.App.Controls", "clr-namespace:GpuTuner.App.Controls;assembly=RochGPU"));
                foreach (var entry in palette)
                    application.Resources[entry.Key] = new SolidColorBrush((Color)ColorConverter.ConvertFromString(dark ? entry.Value.Dark : entry.Value.Light)!);

                var expectedBackground = dark ? Colors.Black : Colors.White;
                var expectedText = dark ? Colors.White : Colors.Black;
                var expectedAccent = dark ? Color.FromRgb(0xFF, 0x5A, 0x5F) : Color.FromRgb(0xB9, 0x1C, 0x1C);
                var expectedBrand = Color.FromRgb(0xD0, 0x34, 0x3A);
                AssertBrush("BgBrush", application.Resources["BgBrush"] as Brush, expectedBackground);
                AssertBrush("TextBrush", application.Resources["TextBrush"] as Brush, expectedText);
                AssertBrush("AccentBrush", application.Resources["AccentBrush"] as Brush, expectedAccent);
                AssertBrush("BrandBrush", application.Resources["BrandBrush"] as Brush, expectedBrand);
                AssertBrush("TitleHoverBrush", application.Resources["TitleHoverBrush"] as Brush,
                    dark ? Color.FromRgb(0x22, 0x22, 0x22) : Color.FromRgb(0xE6, 0xE6, 0xE6));
                AssertBrush("CloseHoverBrush", application.Resources["CloseHoverBrush"] as Brush, Color.FromRgb(0xC4, 0x2B, 0x1C));

                var mainResources = source.Root.Element(Ui + "Window.Resources")!;
                var mainContent = source.Root.Elements().Single(e => e.Name != Ui + "Window.Resources");
                var safeRoot = new XElement(Ui + "Grid", source.Root.Attributes().Where(a => a.IsNamespaceDeclaration),
                    new XElement(Ui + "Grid.Resources", mainResources.Elements()), new XElement(mainContent));
                var content = (Grid)XamlReader.Parse(safeRoot.ToString(SaveOptions.DisableFormatting).Replace(
                    ConverterNamespace, ConverterNamespace + ";assembly=RochGPU"));
                content.DataContext = new MainFixture();
                // A fresh tree per theme avoids stale resource mentors on detached XAML content.
                foreach (System.Collections.DictionaryEntry resource in application.Resources)
                    if (resource.Value is Brush) content.Resources[resource.Key] = resource.Value;
                content.SetResourceReference(Panel.BackgroundProperty, "BgBrush");
                content.SetResourceReference(TextElement.ForegroundProperty, "TextBrush");
                TextElement.SetFontFamily(content, new FontFamily("Consolas"));
                TextElement.SetFontSize(content, 12);
                content.UseLayoutRounding = true;
                TextOptions.SetTextRenderingMode(content, TextRenderingMode.Grayscale);
                TextOptions.SetTextFormattingMode(content, TextFormattingMode.Ideal);

                double width = double.Parse(source.Root.Attribute("Width")!.Value, CultureInfo.InvariantCulture);
                double minimumHeight = double.Parse(source.Root.Attribute("MinHeight")!.Value, CultureInfo.InvariantCulture);
                content.Measure(new Size(width, double.PositiveInfinity));
                content.UpdateLayout();
                // Keep the actual controls; disable commands/actions rather than replacing controls.
                foreach (var button in Descendants(content).OfType<ButtonBase>())
                    if (button is not RepeatButton && button.Content is string label &&
                        new[] { "Apply", "Reset to Defaults", "Save", "Graphics", "Telemetry", "Log", "V/F Curve", "XOC", "Fan" }.Contains(label))
                        button.IsEnabled = false;
                if (content.FindName("ThemeButton") is Button themeButton)
                {
                    // Keep the actual source Canvas/Path in light mode. Code-behind is excluded;
                    // only dark mode's existing plain Unicode target needs a fixture override.
                    if (dark) themeButton.Content = "\u2600";
                    themeButton.ToolTip = dark ? "Switch to light mode" : "Switch to dark mode";
                }
                foreach (var combo in Descendants(content).OfType<ComboBox>()) combo.IsDropDownOpen = false;
                content.Measure(new Size(width, double.PositiveInfinity));
                double contentHeight = Math.Max(minimumHeight, Math.Ceiling(content.DesiredSize.Height));
                var preview = new Grid { UseLayoutRounding = true };
                preview.SetResourceReference(Panel.BackgroundProperty, "BgBrush");
                preview.RowDefinitions.Add(new RowDefinition { Height = new GridLength(48) });
                preview.RowDefinitions.Add(new RowDefinition { Height = new GridLength(contentHeight) });
                var disclosure = new TextBlock { Margin = new Thickness(12, 6, 12, 6), FontFamily = new FontFamily("Consolas"), FontSize = 12, FontWeight = FontWeights.Bold };
                disclosure.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
                preview.Children.Add(disclosure);
                Grid.SetRow(content, 1);
                preview.Children.Add(content);

                foreach (int dpi in Dpis)
                {
                    disclosure.Text = $"FIXTURE PREVIEW - synthetic NVIDIA data - {dpi} DPI export\nOffscreen source XAML; hardware actions disabled";
                    var size = new Size(width, contentHeight + 48);
                    preview.Measure(size);
                    preview.Arrange(new Rect(new Point(), size));
                    preview.UpdateLayout();
                    Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
                    preview.UpdateLayout();
                    AssertNoWindows(application);
                    ValidateControls(content, expectedText, expectedAccent, expectedBrand, dark);
                    AssertBrush("Resolved content background", content.Background, expectedBackground);
                    if (Descendants(content).OfType<ComboBox>().Any(c => c.IsDropDownOpen) || Descendants(content).OfType<Popup>().Any(p => p.IsOpen))
                        throw new InvalidOperationException("Preview popups must stay closed.");
                    var bitmap = new RenderTargetBitmap((int)Math.Ceiling(size.Width * dpi / 96),
                        (int)Math.Ceiling(size.Height * dpi / 96), dpi, dpi, PixelFormats.Pbgra32);
                    bitmap.Render(preview);
                    AssertNoWindows(application);
                    if (PresentationSource.FromVisual(preview) != null || PresentationSource.FromVisual(content) != null)
                        throw new InvalidOperationException("Offscreen roots must not acquire a presentation/HWND source.");
                    var encoder = new PngBitmapEncoder();
                    encoder.Frames.Add(BitmapFrame.Create(bitmap));
                    string path = Path.Combine(directory, $"main-fixture-{(dark ? "dark" : "light")}-{dpi}dpi.png");
                    using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
                    encoder.Save(stream);
                    provenance.Add($"EXPORTED {Path.GetFileName(path)}: {bitmap.PixelWidth}x{bitmap.PixelHeight}; content {width}x{contentHeight} DIP; 7 NVIDIA sliders; 5 slots; actions disabled.");
                }
            }
            using var reportStream = new FileStream(Path.Combine(directory, "main-fixture-provenance.txt"), FileMode.CreateNew, FileAccess.Write, FileShare.None);
            using var writer = new StreamWriter(reportStream);
            foreach (string line in provenance) { writer.WriteLine(line); Console.WriteLine(line); }
        }
        finally { application.Shutdown(); }
    }

    private static void ValidateMainNodes(XElement root)
    {
        foreach (var element in root.DescendantsAndSelf())
            if (!(element.Name.Namespace == Ui && AllowedMainNodes.Contains(element.Name.LocalName)) &&
                !(element.Name.NamespaceName == ConverterNamespace && new[] { "BoolToVisibilityConverter", "BoolToBrushConverter", "InformationLinePartConverter" }.Contains(element.Name.LocalName)))
                throw new InvalidOperationException($"Unreviewed main XAML node: {element.Name}");
        foreach (var attribute in root.DescendantsAndSelf().Attributes())
            if (!attribute.IsNamespaceDeclaration && attribute.Value.Contains("{x:Static", StringComparison.Ordinal))
                throw new InvalidOperationException("Static code references are excluded from the preview.");
    }

    private static void ValidateHoverSources(XElement root, List<string> provenance)
    {
        foreach (var role in new[] { (Style: "TitleButton", Brush: "TitleHoverBrush"), (Style: "TitleCloseButton", Brush: "CloseHoverBrush") })
        {
            var style = root.Descendants(Ui + "Style").Single(e => (string?)e.Attribute(X + "Key") == role.Style);
            bool targetsTemplateChild = style.Descendants(Ui + "Trigger")
                .Where(e => (string?)e.Attribute("Property") == "IsMouseOver" && (string?)e.Attribute("Value") == "True")
                .SelectMany(e => e.Elements(Ui + "Setter"))
                .Any(e => (string?)e.Attribute("TargetName") == "b" && (string?)e.Attribute("Property") == "Background" &&
                    (string?)e.Attribute("Value") == "{DynamicResource " + role.Brush + "}");
            if (!targetsTemplateChild) throw new InvalidOperationException($"{role.Style} must set child b background to {role.Brush} on hover.");
        }
        provenance.Add("HOVER: static source/template role checks only: theme/minimize=TitleHoverBrush; close=CloseHoverBrush. No pointer interaction performed.");
    }

    private static void ValidateControls(Grid content, Color expectedText, Color expectedAccent, Color expectedBrand, bool dark)
    {
        var descendants = Descendants(content).ToArray();
        if (descendants.OfType<Slider>().Count(InLayout) != 7) throw new InvalidOperationException("Expected all seven NVIDIA slider rows.");
        if (descendants.OfType<TextBox>().Count(t => InLayout(t) && t.Name != "PART_EditableTextBox") != 7)
            throw new InvalidOperationException("Expected seven NVIDIA value inputs.");
        if (content.FindName("SlotBar") is not ItemsControl slots || slots.Items.Count != 5)
            throw new InvalidOperationException("Expected all five profile slots.");
        if (!descendants.OfType<CheckBox>().Any(c => Equals(c.Content, "Startup") && c.IsChecked == true))
            throw new InvalidOperationException("Expected fixture Startup checkbox state.");
        if (!descendants.OfType<ComboBox>().Any(c => InLayout(c) && Equals(c.SelectedItem, "Slot 1 - fixture")))
            throw new InvalidOperationException("Expected fixture startup profile selector.");
        foreach (var button in descendants.OfType<Button>().Where(b => Equals(b.Content, "Apply") || Equals(b.Content, "Reset to Defaults")))
            if (button.IsEnabled) throw new InvalidOperationException("Hardware actions must remain disabled.");
        foreach (var value in descendants.OfType<TextBox>().Where(t => InLayout(t) && t.Name != "PART_EditableTextBox"))
            AssertBrush("Value input", value.Foreground, expectedAccent);
        foreach (string text in new[] { "Voltage Boost (%)", "Core Clock (MHz offset)", "XBAR Clock (MHz offset)", "SYS Clock (MHz offset)", "Video Clock (MHz offset)", "Memory Clock (MHz offset)", "Power Limit (%)" })
            AssertBrush(text, descendants.OfType<TextBlock>().Single(t => t.Text == text).Foreground, expectedText);
        foreach (string text in new[] { "YouTube", "X", "Discord" })
            AssertBrush(text, descendants.OfType<TextBlock>().Single(t => t.Text == text).Foreground, expectedBrand);

        var fixture = (MainFixture)content.DataContext;
        foreach (string text in new[] { fixture.TitleText, fixture.AppliedSummary, fixture.Status, fixture.LimitReasonText })
            AssertBrush("Metadata " + text, descendants.OfType<TextBlock>().Single(t => t.Text == text && InLayout(t)).Foreground, expectedText);
        AssertBrush("GPU identity value", descendants.OfType<TextBlock>().Single(t => t.Text == fixture.DeviceDisplayName && InLayout(t)).Foreground, expectedAccent);
        foreach (string text in new[] { fixture.DriverLine, fixture.BiosLine, fixture.MemoryTypeLine, fixture.PcieBusLine, fixture.ResizableBarLine })
        {
            var line = descendants.OfType<TextBlock>().Single(t => t.Text == text && InLayout(t));
            var runs = line.Inlines.OfType<Run>().ToArray();
            if (runs.Length != 2 || string.Concat(runs.Select(run => run.Text)) != text)
                throw new InvalidOperationException("Information label/value runs must preserve the complete original text.");
            AssertBrush("Information label " + text, runs[0].Foreground, expectedText);
            AssertBrush("Information value " + text, runs[1].Foreground, expectedAccent);
        }
        var activeSlot = descendants.OfType<Button>().Single(b => b.DataContext is SlotFixture { IsActive: true });
        AssertBrush("Active profile fill", activeSlot.Background, expectedBrand);
        AssertBrush("Active profile text", activeSlot.Foreground, Colors.White);
        if (activeSlot.FontSize != 13 || activeSlot.FontWeight != FontWeights.Bold)
            throw new InvalidOperationException("Profile tabs must retain 13px bold typography.");
        var startupCombo = descendants.OfType<ComboBox>().Single(c => InLayout(c));
        AssertBrush("Profile combo background", startupCombo.Background, dark ? Colors.Black : Colors.White);
        AssertBrush("Profile combo foreground", startupCombo.Foreground, expectedText);

        var theme = (Button)content.FindName("ThemeButton");
        if (dark) CheckGlyph(theme, "\u2600", 36, 18);
        else
        {
            CheckTitleButton(theme, 36, 18);
            if (theme.Content is not Canvas moon || moon.Width != 18 || moon.Height != 18 ||
                moon.ActualWidth != 18 || moon.ActualHeight != 18 || moon.Children.Count != 1 ||
                moon.Children[0] is not System.Windows.Shapes.Path path)
                throw new InvalidOperationException("Light target must retain the source 18x18 Canvas with one vector Path.");
            AssertBrush("Solid moon fill", path.Fill, Colors.Black);
            if (path.Stroke != null || path.Data == null ||
                path.Data.ToString(CultureInfo.InvariantCulture) != Geometry.Parse(SolidMoonData).ToString(CultureInfo.InvariantCulture))
                throw new InvalidOperationException("Solid moon must use the exact shared geometry with no stroke.");
            var origin = moon.TranslatePoint(new Point(), theme);
            if (Math.Abs(origin.X - 9) > 0.5 || Math.Abs(origin.Y - 6) > 0.5)
                throw new InvalidOperationException("18x18 moon canvas must be centered in the 36x30 theme button.");
        }
        CheckGlyph(descendants.OfType<Button>().Single(b => Equals(b.Content, "\u2212")), "\u2212", 44, 18);
        CheckGlyph(descendants.OfType<Button>().Single(b => Equals(b.Content, "\u00D7")), "\u00D7", 44, 20);
        foreach (var slider in descendants.OfType<Slider>().Where(InLayout))
        {
            var track = (Track)slider.Template.FindName("PART_Track", slider);
            AssertBrush("Slider thumb", Descendants(track.Thumb).OfType<Border>().Single().Background, expectedText);
            var unfilled = Descendants(track.IncreaseRepeatButton).OfType<Border>().Single();
            AssertBrush("Unfilled slider track", unfilled.Background,
                dark ? Color.FromRgb(0x38, 0x38, 0x38) : Color.FromRgb(0xD0, 0xD0, 0xD0));
            if (unfilled.ActualHeight <= 0 || unfilled.ActualWidth <= 0)
                throw new InvalidOperationException("Unfilled slider track must remain visible.");
        }
        // Exercise only a local WPF enabled state, then restore it before capture.
        var input = descendants.OfType<TextBox>().First(t => InLayout(t) && t.Name != "PART_EditableTextBox");
        input.SetCurrentValue(UIElement.IsEnabledProperty, false);
        Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
        AssertBrush("Disabled value input", input.Foreground,
            dark ? Color.FromRgb(0xA0, 0xA0, 0xA0) : Color.FromRgb(0x62, 0x66, 0x6D));
        for (DependencyObject? ancestor = input; ancestor != null; ancestor = VisualTreeHelper.GetParent(ancestor))
            if (ancestor is UIElement visual && visual.Opacity < 0.99)
                throw new InvalidOperationException("Disabled input must use legible color without opacity fading.");
        input.ClearValue(UIElement.IsEnabledProperty);
        Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
        AssertBrush("Restored value input", input.Foreground, expectedAccent);

        void CheckGlyph(Button button, string glyph, double width, double fontSize)
        {
            if (!Equals(button.Content, glyph)) throw new InvalidOperationException($"Expected title glyph {glyph}.");
            CheckTitleButton(button, width, fontSize);
        }
        void CheckTitleButton(Button button, double width, double fontSize)
        {
            if (button.Width != width || button.Height != 30 || button.FontSize != fontSize ||
                button.FontFamily.Source != "Segoe UI Symbol")
                throw new InvalidOperationException("Title control must retain its exact family dimensions and font.");
            AssertBrush("Title control", button.Foreground, expectedText);
            var frame = (Border)button.Template.FindName("b", button);
            AssertBrush("Normal title glyph background", frame.Background, Colors.Transparent);
        }
    }

    private static bool InLayout(DependencyObject element)
    {
        for (DependencyObject? current = element; current != null; current = VisualTreeHelper.GetParent(current))
            if (current is UIElement visual && visual.Visibility != Visibility.Visible) return false;
        return true;
    }
    private static void AssertNoWindows(System.Windows.Application application)
    {
        if (!ReferenceEquals(System.Windows.Application.Current, application) || application.Windows.Count != 0)
            throw new InvalidOperationException("Main fixture preview must have only a base Application and zero windows.");
    }
    private static void AssertBrush(string name, Brush? brush, Color expected)
    {
        if (brush is not SolidColorBrush solid || solid.Color != expected)
            throw new InvalidOperationException($"{name} must resolve to {expected}; actual={brush}.");
    }
    private static string Hash(string path) => Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(path)));
    private static IEnumerable<string> OutputPaths(string directory) =>
        (from dark in new[] { false, true } from dpi in Dpis select Path.Combine(directory, $"main-fixture-{(dark ? "dark" : "light")}-{dpi}dpi.png"))
        .Append(Path.Combine(directory, "main-fixture-provenance.txt"));
    private static IEnumerable<DependencyObject> Descendants(DependencyObject parent)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            var child = VisualTreeHelper.GetChild(parent, i);
            yield return child;
            foreach (var nested in Descendants(child)) yield return nested;
        }
    }

    private sealed class DisabledCommand : ICommand
    {
        public bool CanExecute(object? parameter) => false;
        public void Execute(object? parameter) => throw new InvalidOperationException("Fixture commands cannot execute.");
        public event EventHandler? CanExecuteChanged { add { } remove { } }
    }
    private sealed class InertProfileCommand : ICommand
    {
        // Preserve the production profile-state appearance without a real profile action.
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => throw new InvalidOperationException("Fixture profile commands cannot execute.");
        public event EventHandler? CanExecuteChanged { add { } remove { } }
    }
    private sealed class MainFixture
    {
        public string TitleText { get; set; } = "Roch GPU (fixture)";
        public string DeviceDisplayName { get; set; } = "NVIDIA GeForce RTX 5090 (illustrative fixture)";
        public string DriverLine { get; set; } = "Driver: EXAMPLE-DRIVER";
        public string BiosLine { get; set; } = "VBIOS: EXAMPLE-VBIOS";
        public string MemoryTypeLine { get; set; } = "Memory: GDDR7 (fixture)";
        public string PcieBusLine { get; set; } = "PCIe: illustrative bus interface";
        public string ResizableBarLine { get; set; } = "Resizable BAR: Enabled (fixture)";
        public string Status { get; set; } = "Fixture preview - hardware actions disabled";
        public string AppliedSummary { get; set; } = "Synthetic values; no GPU was queried.";
        public bool StatusIsError { get; set; }
        public bool LimitIsActive { get; set; }
        public string LimitReasonText { get; set; } = "No active limit (fixture)";
        public bool HasVoltageBoost { get; set; } = true;
        public bool HasXbar { get; set; } = true;
        public bool HasSys { get; set; } = true;
        public bool HasVideo { get; set; } = true;
        public bool HasCurveEditor { get; set; } = true;
        public bool HasXoc { get; set; } = true;
        public bool HasTempLimit { get; set; }
        public bool HasMemoryTiming { get; set; }
        public bool HasIntelClockRange { get; set; }
        public bool IsVoltageOffset { get; set; }
        public bool IntelClockRangeEnabled { get; set; }
        public bool SaveArmed { get; set; }
        public bool PendingChanges { get; set; }
        public bool ApplyOnStartup { get; set; } = true;
        public string[] Profiles { get; set; } = ["Slot 1 - fixture", "Slot 2 - fixture", "Slot 3 - fixture", "Slot 4 - fixture", "Slot 5 - fixture"];
        public string StartupProfile { get; set; } = "Slot 1 - fixture";
        public SlotFixture[] Slots { get; set; } = Enumerable.Range(1, 5).Select(n => new SlotFixture { Number = n, Label = n.ToString(), Occupied = n <= 2, IsActive = n == 1 }).ToArray();
        public CapsFixture Caps { get; set; } = new();
        public ICommand ApplyCommand { get; } = new DisabledCommand();
        public ICommand ResetCommand { get; } = new DisabledCommand();
        public ICommand NudgeCommand { get; } = new DisabledCommand();
        public ICommand SlotCommand { get; } = new InertProfileCommand();
        public string VoltageBoostLabel { get; set; } = "Voltage Boost (%)";
        public string CoreLabel { get; set; } = "Core Clock (MHz offset)";
        public string MemoryLabel { get; set; } = "Memory Clock (MHz offset)";
        public string PowerLabel { get; set; } = "Power Limit (%)";
        public string TempLabel { get; set; } = "Temp. Limit (C)";
        public double VoltageBoost { get; set; } = 10;
        public double VoltageOffset { get; set; }
        public double CoreOffset { get; set; } = 150;
        public double XbarOffset { get; set; } = 30;
        public double SysOffset { get; set; } = 15;
        public double VideoOffset { get; set; }
        public double MemoryOffset { get; set; } = 500;
        public double PowerLimit { get; set; } = 100;
        public double TempLimit { get; set; } = 83;
        public double CoreStepMhz { get; set; } = 15;
        public double MemoryStepMhz { get; set; } = 100;
        public double PowerStepPercent { get; set; } = 1;
        public double TempStep { get; set; } = 1;
        public double VoltageBoostStepPercent { get; set; } = 1;
        public string VoltageBoostInput { get; set; } = "10";
        public string VoltageOffsetInput { get; set; } = "0";
        public string CoreOffsetInput { get; set; } = "150";
        public string XbarOffsetInput { get; set; } = "30";
        public string SysOffsetInput { get; set; } = "15";
        public string VideoOffsetInput { get; set; } = "0";
        public string MemoryOffsetInput { get; set; } = "500";
        public string PowerLimitInput { get; set; } = "100";
        public string TempLimitInput { get; set; } = "83";
        public string ClockLockMinInput { get; set; } = "500";
        public string ClockLockMaxInput { get; set; } = "3000";
        public string VoltageBoostRangeText { get; set; } = "0 to 100 (fixture)";
        public string VoltageOffsetRangeText { get; set; } = "-200 to 200 (fixture)";
        public string CoreRangeText { get; set; } = "-500 to 1500 (fixture)";
        public string XbarOffsetRangeText { get; set; } = "-500 to 1500 (fixture)";
        public string SysOffsetRangeText { get; set; } = "-500 to 1500 (fixture)";
        public string VideoOffsetRangeText { get; set; } = "-500 to 1500 (fixture)";
        public string MemRangeText { get; set; } = "-1000 to 4000 (fixture)";
        public string PowerRangeText { get; set; } = "50 to 120 (fixture)";
        public string TempRangeText { get; set; } = "60 to 95 (fixture)";
        public string ClockLockRangeText { get; set; } = "500 to 3000 (fixture)";
        public string[] MemoryTimingOptions { get; set; } = ["Default (fixture)"];
        public int MemoryTimingIndex { get; set; }
    }
    private sealed class SlotFixture
    {
        public int Number { get; set; }
        public string Label { get; set; } = "";
        public string Tip { get; set; } = "Illustrative slot; profile actions disabled";
        public bool Occupied { get; set; }
        public bool IsActive { get; set; }
    }
    private sealed class CapsFixture
    {
        public bool CanSetCoreOffset { get; set; } = true;
        public bool CanSetMemoryOffset { get; set; } = true;
        public bool CanSetPowerLimit { get; set; } = true;
        public bool CanSetFanSpeed { get; set; } = true;
        public double VoltageBoostMinPercent { get; set; }
        public double VoltageBoostMaxPercent { get; set; } = 100;
        public double VoltageOffsetMinMv { get; set; } = -200;
        public double VoltageOffsetMaxMv { get; set; } = 200;
        public double CoreOffsetMinMhz { get; set; } = -500;
        public double CoreOffsetMaxMhz { get; set; } = 1500;
        public double XbarOffsetMinMhz { get; set; } = -500;
        public double XbarOffsetMaxMhz { get; set; } = 1500;
        public double SysOffsetMinMhz { get; set; } = -500;
        public double SysOffsetMaxMhz { get; set; } = 1500;
        public double VideoOffsetMinMhz { get; set; } = -500;
        public double VideoOffsetMaxMhz { get; set; } = 1500;
        public double MemoryOffsetMinMhz { get; set; } = -1000;
        public double MemoryOffsetMaxMhz { get; set; } = 4000;
        public double PowerLimitMinPercent { get; set; } = 50;
        public double PowerLimitMaxPercent { get; set; } = 120;
        public double TempLimitMinC { get; set; } = 60;
        public double TempLimitMaxC { get; set; } = 95;
    }
}
