using System.Diagnostics;
using System.Runtime.InteropServices;

namespace GpuTuner.Core.Services;

/// <summary>
/// Registers a Windows Task Scheduler entry that runs the app elevated at logon with
/// "--apply-profile &lt;name&gt; --exit" (or without --exit if you want it to stay resident).
/// Uses schtasks.exe so we need no COM interop or NuGet packages. Windows-only.
/// </summary>
public static class StartupTaskService
{
    public const string TaskName = "ROCH GPU Apply Profile";

    public static bool IsWindows => RuntimeInformation.IsOSPlatform(OSPlatform.Windows);

    public static bool Exists()
    {
        if (!IsWindows) return false;
        var (code, _) = Run("schtasks", $"/Query /TN \"{TaskName}\"");
        return code == 0;
    }

    /// <summary>Create/replace the task. exePath must be the full path to the GUI/CLI executable.</summary>
    public static void Register(string exePath, string profileName, bool stayResident)
    {
        if (!IsWindows) throw new PlatformNotSupportedException();
        // /RL HIGHEST = run elevated; /SC ONLOGON; /DELAY gives the driver a few seconds to settle.
        string args = $"--apply-profile \"{profileName}\"" + (stayResident ? " --minimized" : " --exit");
        string tr = $"\\\"{exePath}\\\" {args.Replace("\"", "\\\"")}";
        var (code, output) = Run("schtasks",
            $"/Create /F /TN \"{TaskName}\" /SC ONLOGON /RL HIGHEST /DELAY 0000:15 /TR \"{tr}\"");
        if (code != 0) throw new InvalidOperationException("schtasks failed: " + output);
    }

    public static void Unregister()
    {
        if (!IsWindows) return;
        Run("schtasks", $"/Delete /F /TN \"{TaskName}\"");
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
