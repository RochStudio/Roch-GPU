using GpuTuner.App.ViewModels;
using GpuTuner.Core.Models;

int pass = 0, fail = 0;
void Check(string name, bool success) { if (success) pass++; else { fail++; Console.WriteLine("FAIL: " + name); } }
var empty = new GpuTelemetry { HotSpotC = double.NaN, MemoryTemperatureC = double.NaN, VoltageMv = double.NaN, PowerWatts = double.NaN, PowerPercent = double.NaN };
var table = new TelemetryTable(empty, new GpuCapabilities { PowerLimitIsOffset = true });
Check("column labels are not repeated inside the scroll body", !table.Rows.Any(r => r.IsColumnHeader));
Check("no unsupported effective clock", !table.Rows.Any(r => r.Name.Contains("effective")));
Check("no unavailable power utilization placeholder", !table.Rows.Any(r => r.Name.Contains("TDP") || r.Name.Contains("unavailable")));
var sample = empty with
{
    FabricClockMhz = 2401, SocClockMhz = 1371, PowerWatts = 34, MemoryUsedMb = 292,
    SupplementalSensors = new SupplementalSensor[]
    {
        new("adlx:shared", "Shared GPU memory used", "MB", 91),
        new("adl:10", "Core VRM", "°C", 43),
        new("adl:16", "SoC voltage", "mV", 940),
        new("adl:41", "PCIe link width", "lanes", 16)
    }
};
table.Add(sample);
Check("late FCLK discovery", table.Rows.Single(r => r.Name == "Fabric (FCLK)").Current == "2401 MHz");
Check("late SoC discovery", table.Rows.Single(r => r.Name == "SoC").Current == "1371 MHz");
Check("VRAM updates existing row", table.Rows.Single(r => r.Name == "Dedicated VRAM used").Current == "292 MB");
string GroupOf(string name)
{
    int i = table.Rows.ToList().FindIndex(r => r.Name == name);
    return table.Rows.Take(i).Last(r => r.IsHeader).Name;
}
Check("extra memory grouped", GroupOf("Shared GPU memory used") == "MEMORY");
Check("extra temperature grouped", GroupOf("Core VRM") == "TEMPERATURES");
Check("extra voltage grouped", GroupOf("SoC voltage") == "VOLTAGES");
Check("extra PCIe grouped", GroupOf("PCIe link width") == "PCIE LINK");
int count = table.Rows.Count;
table.Add(sample);
Check("no duplicate rows on repeated polls", count == table.Rows.Count);
table.Add(empty);
var shared = table.Rows.Single(r => r.Name == "Shared GPU memory used");
Check("disconnected sensor current cleared", shared.Current == "—");
Check("disconnected sensor history retained", shared.Minimum == "91 MB" && shared.Maximum == "91 MB");
Check("disconnected late-discovered clock cleared", table.Rows.Single(r => r.Name == "Fabric (FCLK)").Current == "—");
table.ResetStats();
Check("reset clears history", shared.Average == "—" && shared.Minimum == "—");
table.Add(sample);
Check("sensor can recover", shared.Current == "91 MB");
bool banding = true;
int data = 0;
foreach (var row in table.Rows)
{
    if (row.IsHeader || row.IsColumnHeader) data = 0;
    else { banding &= row.IsBanded == (data % 2 == 1); data++; }
}
Check("inserted sensors preserve striping", banding);
Console.WriteLine($"Telemetry: {pass} passed, {fail} failed");
Environment.ExitCode = fail == 0 ? 0 : 1;
