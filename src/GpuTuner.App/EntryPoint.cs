using System;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;
using System.Security.Principal;

namespace GpuTuner.App;

/// <summary>
/// One executable, two front ends.
///
/// A verb ("info", "apply", ...) runs the command line; no arguments, or the window's own --flags,
/// runs the window. The two never collide because CLI verbs never begin with a dash and every GUI
/// flag does — which matters more than it looks, since the logon task launches the window with
/// --minimized and --apply-profile.
/// </summary>
public static class EntryPoint
{
    [DllImport("kernel32.dll")]
    private static extern bool AttachConsole(int processId);
    private const int AttachParentProcess = -1;

    [STAThread]
    public static int Main(string[] args)
    {
        if (WantsCommandLine(args))
        {
            // A WinExe owns no console of its own, so borrow whichever one launched it. Without
            // this every line the CLI prints goes nowhere and the command looks like it did nothing.
            AttachConsole(AttachParentProcess);
            return Cli.CommandLine.Run(args);
        }

        // The window writes clocks, so it wants elevation — but asking for it in the manifest would
        // break the other half. A manifest-elevated process is launched fresh and does not inherit
        // the caller's console, so `ROCH GPU.exe info` would print into a console nobody can see.
        // Elevating here, and only for the window, keeps both halves usable.
        if (!IsElevated() && RelaunchElevated(args)) return 0;

        var app = new App();
        app.InitializeComponent();
        return app.Run();
    }

    /// <summary>Help is the one word both halves answer to; the CLI's version is the useful one.</summary>
    private static bool WantsCommandLine(string[] args) =>
        args.Length > 0 && (!args[0].StartsWith('-') || args[0] is "-h" or "--help");

    private static bool IsElevated()
    {
        using var id = WindowsIdentity.GetCurrent();
        return new WindowsPrincipal(id).IsInRole(WindowsBuiltInRole.Administrator);
    }

    /// <summary>
    /// Returns true when a elevated copy has been started and this one should quit. False means the
    /// prompt was declined, in which case running on unelevated is better than exiting silently:
    /// the monitor still works, and every write fails with a message that says why.
    /// </summary>
    private static bool RelaunchElevated(string[] args)
    {
        try
        {
            Process.Start(new ProcessStartInfo(Environment.ProcessPath!)
            {
                UseShellExecute = true,
                Verb = "runas",
                Arguments = string.Join(' ', args.Select(a => a.Contains(' ') ? $"\"{a}\"" : a))
            });
            return true;
        }
        catch (Exception)
        {
            return false;
        }
    }
}
