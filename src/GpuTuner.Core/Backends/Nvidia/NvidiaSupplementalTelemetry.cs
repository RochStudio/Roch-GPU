using GpuTuner.Core.Models;
using NvAPIWrapper.GPU;
using NvAPIWrapper.Native.Exceptions;
using NvAPIWrapper.Native.GPU;

namespace GpuTuner.Core.Backends.Nvidia;

internal static class NvidiaSupplementalTelemetry
{
    internal static double PcieSpeed(int generation) => generation switch
    { 1 => 2.5, 2 => 5, 3 => 8, 4 => 16, 5 => 32, 6 => 64, _ => double.NaN };

    internal static IReadOnlyList<SupplementalSensor> Read(PhysicalGPU gpu, int nvmlIndex)
    {
        var sensors = new List<SupplementalSensor>();
        void Add(string key, string name, string unit, double value)
        {
            if (double.IsFinite(value) && value >= 0)
                sensors.Add(new("nv:" + key, name, unit, value));
        }
        void ReadOptional(Action read)
        {
            try { read(); }
            catch (NVIDIAApiException) { }
            catch (NVIDIANotSupportedException) { }
        }
        ReadOptional(() =>
        {
            foreach (var usage in gpu.UsageInformation.UtilizationDomainsStatus)
            {
                if (usage.Domain == UtilizationDomain.VideoEngine)
                    Add("video-load", "Video engine", "%", usage.Percentage);
                if (usage.Domain == UtilizationDomain.BusInterface)
                    Add("bus-load", "Bus interface", "%", usage.Percentage);
            }
        });
        ReadOptional(() =>
        {
            var memory = gpu.MemoryInformation;
            double total = memory.AvailableDedicatedVideoMemoryInkB;
            double free = memory.CurrentAvailableDedicatedVideoMemoryInkB;
            if (total > 0 && free <= total)
            {
                Add("memory-free", "Available VRAM", "MB", free / 1024);
                Add("memory-utilization", "VRAM usage", "%", (total - free) * 100 / total);
            }
        });
        ReadOptional(() =>
        {
            var video = gpu.CurrentClockFrequencies.VideoDecodingClock;
            if (video.IsPresent) Add("video-clock", "Video clock (reported)", "MHz", video.Frequency / 1000.0);
        });
        ReadOptional(() =>
        {
            var thermal = gpu.PerformanceControl.ThermalLimitPolicies.FirstOrDefault();
            if (thermal != null) Add("thermal-limit", "GPU thermal limit", "°C", thermal.TargetTemperature);
        });
        ReadOptional(() =>
        {
            var flags = gpu.PerformanceControl.CurrentActiveLimit;
            foreach (var (flag, key, name) in new[]
            {
                (PerformanceLimit.PowerLimit, "power", "Power limit active"),
                (PerformanceLimit.TemperatureLimit, "thermal", "Thermal limit active"),
                (PerformanceLimit.VoltageLimit, "voltage", "Voltage limit active"),
                (PerformanceLimit.NoLoadLimit, "idle", "No-load limit active")
            }) Add("limit-" + key, name, "state", (flags & flag) != 0 ? 1 : 0);
        });
        var pcie = Nvml.PcieLink(nvmlIndex);
        if (pcie.CurrentGeneration > 0)
        {
            Add("pcie-generation", "PCIe generation", "gen", pcie.CurrentGeneration);
            Add("pcie-speed", "PCIe link speed", "GT/s", PcieSpeed(pcie.CurrentGeneration));
        }
        if (pcie.CurrentWidth > 0) Add("pcie-width", "PCIe link width", "lanes", pcie.CurrentWidth);
        return sensors;
    }
}

