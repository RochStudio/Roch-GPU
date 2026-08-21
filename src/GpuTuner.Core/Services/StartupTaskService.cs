using System.Diagnostics;
using System.Runtime.InteropServices;

namespace GpuTuner.Core.Services;

/// <summary>
/// Registers a Windows Task Scheduler entry that runs the app elevated at logon with
/// "--apply-profile &lt;name&gt; --exit": apply the profile, then quit.
/// Uses schtasks.exe so we need no COM interop or NuGet packages. Windows-only.
/// </summary>
public static class StartupTaskService
{
    public const string TaskName = "Roch GPU Apply Profile";

    public static bool IsWindows => RuntimeInformation.IsOSPlatform(OSPlatform.Windows);

    public static bool Exists()
    {
        if (!IsWindows) return false;
        var (code, _) = Run("schtasks", $"/Query /TN \"{TaskName}\"");
        return code == 0;
    }

    /// <summary>
    /// Create/replace the task. exePath must be the full path to the executable.
    ///
    /// Always --exit: apply the profile and quit, leaving nothing resident. The window and the
    /// CLI both register through here, and while they disagreed about this the task quietly
    /// changed behaviour depending on which of the two had been used last.
    ///
    /// The cost is that a *software* fan curve (NVIDIA) cannot run at logon, because no process
    /// is left to step it. Clocks, limits and voltages all stick, and AMD's fan curve is the
    /// driver's own so it is unaffected. Add a resident mode back here if that is ever wanted.
    /// </summary>
    public static void Register(string exePath, string profileName)
    {
        if (!IsWindows) throw new PlatformNotSupportedException();

        // Delete any existing entry first rather than leaning on /Create /F to replace it. Task names
        // match case-insensitively, so one registered under the old spelling ("ROCH GPU Apply
        // Profile") is found and overwritten either way - but Windows keeps the name the task was
        // first created with, so the old casing would survive every rewrite. Removing it makes this
        // create the one that decides the spelling.
        if (Exists()) Run("schtasks", $"/Delete /F /TN \"{TaskName}\"");

        // /RL HIGHEST = run elevated; /SC ONLOGON; /DELAY gives the driver a few seconds to settle.
        string args = $"--apply-profile \"{profileName}\" --exit";
        string tr = $"\\\"{exePath}\\\" {args.Replace("\"", "\\\"")}";
        var (code, output) = Run("schtasks",
            $"/Create /F /TN \"{TaskName}\" /SC ONLOGON /RL HIGHEST /DELAY 0000:15 /TR \"{tr}\"");
        if (code != 0) throw new InvalidOperationException("schtasks failed: " + output);
    }

    /// <summary>
    /// Remove the task. Deleting one that was never registered is success, not failure - but a
    /// delete that reports success while the task survives is not, so the outcome is verified
    /// rather than assumed. Discarding the exit code here let the UI tick the box off and report
    /// "removed" while the task stayed registered and kept firing at every logon.
    /// </summary>
    public static void Unregister()
    {
        if (!IsWindows) return;
        if (!Exists()) return;
        var (code, output) = Run("schtasks", $"/Delete /F /TN \"{TaskName}\"");
        if (Exists())
            throw new InvalidOperationException($"schtasks could not remove the task (exit {code}): {output}");
    }

    private static (int code, string output) Run(string file, string args)
    {
        try
        {
            var psi = new ProcessStartInfo(file, args)
            {
                RedirectStandardOutput = true, RedirectStandardError = true,
                UseShellExecute = false, CreateNoWindow = true
            };
            using var p = Process.Start(psi)!;
            string o = p.StandardOutput.ReadToEnd() + p.StandardError.ReadToEnd();
            p.WaitForExit(10000);
            return (p.ExitCode, o);
        }
        catch (Exception e) { return (-1, e.Message); }
    }
}
