namespace GpuTuner.Core.Backends.Nvidia;

internal static class OcpWriteGuard
{
    internal static string? Apply(uint driverVersion, int currentMa, int requestedMa, Func<int> write)
    {
        if (currentMa <= 0 || requestedMa <= 0)
            return "Cannot verify a positive OCP limit; no write attempted.";
        // Reading an already-correct limit is sufficient, even if the setter is unavailable.
        if (currentMa == requestedMa) return null;
        // The modern path has its own full-payload validation. This guard is legacy-only.
        if (driverVersion == 0 || driverVersion >= 61500)
            return "Legacy OCP writes are unavailable on this driver; the validated modern layout is required. No write attempted.";
        int status = write();
        return status == 0 ? null : $"nvapi 0xAFFC2279: status {status}";
    }
}
