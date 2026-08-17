using GpuTuner.Core.Backends.Amd;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Backends.Nvidia;

namespace GpuTuner.Core.Backends;

/// <summary>
/// Picks the backend for whatever card is in the machine.
///
/// Detection is by attempting initialisation, not by sniffing device IDs: a vendor library that
/// loads, initialises and enumerates at least one GPU is the definition of "this backend works
/// here". Each attempt's failure is kept so a machine with neither card can be told exactly what
/// was tried and why it didn't take.
/// </summary>
public static class BackendFactory
{
    public sealed record Attempt(string Name, string Error);

    /// <summary>
    /// Initialise the first backend that works. Throws <see cref="GpuBackendException"/> listing
    /// every attempt when none do.
    /// </summary>
    public static IGpuBackend CreateAndInitialize(out IReadOnlyList<Attempt> attempts)
    {
        var tried = new List<Attempt>();
        attempts = tried;

        foreach (var make in new Func<IGpuBackend>[] { () => new NvApiBackend(), () => new AdlBackend() })
        {
            IGpuBackend? backend = null;
            try
            {
                backend = make();
                backend.Initialize();
                if (backend.Devices.Count > 0) return backend;
                tried.Add(new Attempt(backend.BackendName, "initialised but reported no GPUs"));
            }
            catch (Exception e)
            {
                tried.Add(new Attempt(backend?.BackendName ?? "unknown", e.Message));
            }
            try { backend?.Dispose(); } catch { }
        }

        throw new GpuBackendException(
            "No supported GPU found. Tried:" + Environment.NewLine +
            string.Join(Environment.NewLine, tried.Select(a => $"  {a.Name}: {a.Error}")));
    }
}
