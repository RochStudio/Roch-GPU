using Microsoft.Win32;
using System.Globalization;

namespace GpuTuner.Core.Models;

/// <summary>One read-only row in the Graphics window.</summary>
public sealed record GpuGraphicsRow(string Name, string Value, bool IsBanded);

/// <summary>
/// Static graphics-card identity. These values describe the installed card and driver; unlike
/// telemetry they do not change while the application is running.
/// </summary>
public sealed record GpuGraphicsInfo
{
    public string Gpu { get; init; } = "";
    public string BoardManufacturer { get; init; } = "";
    public string CodeName { get; init; } = "";
    public string Revision { get; init; } = "";
    public string Cores { get; init; } = "";
    public string RopsTmus { get; init; } = "";
    public string Technology { get; init; } = "";
    public string MemorySize { get; init; } = "";
    public string MemoryType { get; init; } = "";
    public string MemoryVendor { get; init; } = "";
    public string BusWidth { get; init; } = "";
    public string BusInterface { get; init; } = "";
    public string ResizableBar { get; init; } = "";
    public string DriverVersion { get; init; } = "";
    public string DriverDate { get; init; } = "";

    public static GpuGraphicsInfo FromDevice(GpuDevice device) => new()
    {
        Gpu = device.Name,
        MemorySize = device.VramMegabytes > 0
            ? $"{device.VramMegabytes / 1024.0:0.##} GB"
            : "",
        DriverVersion = device.DriverVersion,
        DriverDate = WindowsDisplayIdentity.DriverDate(device.Vendor, device.DriverVersion),
    };

    /// <summary>The same field order as Roch Viewer's System Info / Graphics section.</summary>
    public IReadOnlyList<GpuGraphicsRow> Rows
    {
        get
        {
            var values = new (string Name, string Value)[]
            {
                ("GPU", Gpu),
                ("Board Manufacturer", BoardManufacturer),
                ("GPU Code Name", CodeName),
                ("GPU Revision", Revision),
                ("Cores", Cores),
                ("ROPs / TMUs", RopsTmus),
                ("GPU Technology", Technology),
                ("Memory Size", MemorySize),
                ("Memory Type", MemoryType),
                ("Memory Vendor", MemoryVendor),
                ("Bus Width", BusWidth),
                ("Bus Interface", BusInterface),
                ("Resizable BAR", ResizableBar),
                ("Driver Version", DriverVersion),
                ("Driver Date", DriverDate),
            };
            return values.Select((row, index) =>
                new GpuGraphicsRow(row.Name, Display(row.Value), index % 2 == 1)).ToArray();
        }
    }

    public static string Display(string? value) =>
        string.IsNullOrWhiteSpace(value) ? "—" : value.Trim();
}

/// <summary>Small shared decoders for identifiers read by the vendor backends.</summary>
public static class GpuIdentity
{
    private static readonly IReadOnlyDictionary<int, string> BoardVendors =
        new Dictionary<int, string>
        {
            [0x1043] = "ASUSTeK Computer",
            [0x1458] = "GIGABYTE Technology",
            [0x1462] = "MSI",
            [0x148C] = "PowerColor",
            [0x1682] = "XFX",
            [0x196E] = "PNY",
            [0x19DA] = "Zotac",
            [0x1B4C] = "KFA2",
            [0x1DA2] = "Sapphire Technology",
            [0x3842] = "EVGA",
            [0x7377] = "Colorful",
            [0x10DE] = "NVIDIA",
            [0x1002] = "AMD",
        };

    public static string BoardVendor(int id) =>
        BoardVendors.TryGetValue(id, out var name) ? name : id > 0 ? $"0x{id:X4}" : "";

    public static string DriverDateText(string? value)
    {
        string raw = (value ?? "").Trim();
        if (raw.Length >= 8 && raw.Take(8).All(char.IsDigit))
            return $"{raw[..4]}-{raw.Substring(4, 2)}-{raw.Substring(6, 2)}";
        if (DateTime.TryParse(raw, CultureInfo.InvariantCulture,
                DateTimeStyles.AllowWhiteSpaces, out var parsed) ||
            DateTime.TryParse(raw, out parsed))
            return parsed.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
        return "";
    }

    /// <summary>
    /// Maximum PCIe link capability. Every number is supplied by the vendor driver; the currently
    /// power-saving link is deliberately omitted so the header stays stable while the GPU idles.
    /// </summary>
    public static string PcieBusInterface(
        int maximumGeneration, int maximumWidth,
        int currentGeneration, int currentWidth)
    {
        static string Half(int generation, int width)
        {
            if (generation <= 0 && width <= 0) return "";
            if (generation <= 0) return $"x{width}";
            if (width <= 0) return $"{generation}.0";
            return $"{generation}.0 x{width}";
        }

        string maximum = Half(maximumGeneration, maximumWidth);
        return maximum.Length > 0 ? maximum : Half(currentGeneration, currentWidth);
    }
}

/// <summary>Best-effort installed-driver metadata from Windows' display-class registry.</summary>
internal static class WindowsDisplayIdentity
{
    private const string DisplayClass =
        @"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    public static string DriverDate(string vendor, string version)
    {
        if (!OperatingSystem.IsWindows()) return "";
        try
        {
            using var root = Registry.LocalMachine.OpenSubKey(DisplayClass);
            if (root == null) return "";
            foreach (string childName in root.GetSubKeyNames())
            {
                using var child = root.OpenSubKey(childName);
                if (child == null) continue;
                string provider = child.GetValue("ProviderName") as string ?? "";
                string installedVersion = child.GetValue("DriverVersion") as string ?? "";
                bool vendorMatch = vendor.Equals("NVIDIA", StringComparison.OrdinalIgnoreCase)
                    ? provider.Contains("NVIDIA", StringComparison.OrdinalIgnoreCase)
                    : provider.Contains("AMD", StringComparison.OrdinalIgnoreCase)
                      || provider.Contains("Advanced Micro Devices", StringComparison.OrdinalIgnoreCase);
                bool versionMatch = version.Length > 0 && installedVersion.Contains(
                    version, StringComparison.OrdinalIgnoreCase);
                if (!vendorMatch && !versionMatch) continue;
                string date = GpuIdentity.DriverDateText(child.GetValue("DriverDate")?.ToString());
                if (date.Length > 0) return date;
            }
        }
        catch { }
        return "";
    }
}
