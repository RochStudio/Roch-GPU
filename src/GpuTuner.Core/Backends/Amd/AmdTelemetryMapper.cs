using GpuTuner.Core.Models;

namespace GpuTuner.Core.Backends.Amd;

/// <summary>Pure mapping of supported driver samples; never substitutes zero for a missing sensor.</summary>
internal static class AmdTelemetryMapper
{
    // AMD adl_defines.h ADL_PMLOG_SENSORS. Reserved, CPU/APU and undocumented-unit
    // fields are deliberately excluded. Clocks are MHz, voltage mV, power W.
    internal sealed record Sensor(int Id, string Name, string Unit, bool PositiveOnly = false);
    internal static readonly Sensor[] Additional =
    {
        new(10, "Core VRM", "°C"), new(11, "Memory VRM", "°C"),
        new(24, "SoC VRM", "°C"), new(25, "Memory VRM 0", "°C"), new(26, "Memory VRM 1", "°C"),
        new(12, "Liquid", "°C"), new(13, "PCIe bridge", "°C"),
        new(28, "Graphics die", "°C"), new(29, "SoC", "°C"),
        new(42, "Liquid 0", "°C"), new(43, "Liquid 1", "°C"),
        new(50, "GCD hot spot", "°C"), new(51, "MCD hot spot", "°C"),
        new(16, "SoC voltage", "mV", true), new(22, "Memory voltage", "mV", true),
        new(4, "UVD clock 1", "MHz"), new(5, "UVD clock 2", "MHz"),
        new(6, "VCE clock", "MHz"), new(7, "VCN clock", "MHz"),
        new(36, "VCN1 clock 1", "MHz"), new(37, "VCN1 clock 2", "MHz"),
        new(30, "Graphics core power", "W"), new(17, "SoC power", "W"),
        new(49, "dGPU power limit", "W", true),
        new(52, "Graphics thermal throttling", "%"), new(53, "Memory thermal throttling", "%"),
        new(54, "VRM thermal throttling", "%"), new(55, "Power throttling", "%"),
        new(56, "Current throttling", "%"), new(57, "Voltage throttling", "%"),
        new(40, "PCIe link generation", "gen", true), new(41, "PCIe link width", "lanes", true),
        new(58, "PCIe maximum generation", "gen", true)
    };

    internal static double Valid(double value, string unit, bool positiveOnly = false)
    {
        if (!double.IsFinite(value) || value < 0 || (positiveOnly && value == 0)) return double.NaN;
        if (unit == "°C" && value > 256 || unit == "%" && value > 100) return double.NaN;
        return value;
    }

    public static GpuTelemetry Map(IReadOnlyDictionary<int, double> pm, IReadOnlyDictionary<string, double> adlx)
    {
        double P(int id) => pm.TryGetValue(id, out double v) ? v : double.NaN;
        double A(string key) => adlx.TryGetValue(key, out double v) ? v : double.NaN;
        double Read(int id, string key, string unit, bool positive = false)
        {
            double v = Valid(P(id), unit, positive);
            return double.IsFinite(v) ? v : Valid(A(key), unit, positive);
        }
        var extra = new List<SupplementalSensor>();
        foreach (var d in Additional)
        {
            double value = Valid(P(d.Id), d.Unit, d.PositiveOnly);
            if (double.IsFinite(value)) extra.Add(new($"adl:{d.Id}", d.Name, d.Unit, value));
        }
        void Extra(string key, string name, string unit, double value)
        {
            value = Valid(value, unit);
            if (double.IsFinite(value)) extra.Add(new(key, name, unit, value));
        }
        Extra("amd:asic", "GPU ASIC power", "W", Read(23, "power", "W"));
        Extra("amd:intake", "Intake", "°C", Read(74, "intake", "°C"));
        Extra("adlx:shared", "Shared GPU memory used", "MB", A("shared"));
        double fan = Read(14, "fan", "RPM"), duty = Read(15, "fanduty", "%");
        return new GpuTelemetry
        {
            CoreClockMhz = Read(1, "core", "MHz"), MemoryClockMhz = Read(2, "memoryclock", "MHz"),
            FabricClockMhz = Valid(P(44), "MHz"), SocClockMhz = Valid(P(3), "MHz"),
            TemperatureC = Read(8, "temperature", "°C"), HotSpotC = Read(27, "hotspot", "°C"),
            MemoryTemperatureC = Read(9, "memorytemp", "°C"), VoltageMv = Read(21, "voltage", "mV", true),
            // ASIC power is NOT board draw. Keep separate even when board power is missing.
            PowerWatts = Read(73, "board", "W"), PowerPercent = double.NaN,
            GpuLoadPercent = Read(19, "usage", "%"), MemoryLoadPercent = Valid(P(20), "%"),
            MemoryUsedMb = Valid(A("vram"), "MB"),
            FanRpm = fan, FanPercent = duty, FanRpms = new[] { fan }, FanPercents = new[] { duty },
            SupplementalSensors = extra
        };
    }
}
