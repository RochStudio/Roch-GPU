using System;
using System.Diagnostics;
using System.IO;
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
            AttachToOutput();
            return Cli.CommandLine.Run(args);
        }

        // The window writes clocks, so it wants elevation — but asking for it in the manifest would
        // break the other half. A manifest-elevated process is launched fresh and does not inherit
        // the caller's console, so `RochGPU.exe info` would print into a console nobody can see.
        // Elevating here, and only for the window, keeps both halves usable.
        if (!IsElevated() && RelaunchElevated(args)) return 0;

        var app = new App();
        app.InitializeComponent();
        return app.Run();
    }

    /// <summary>
    /// Point Console at wherever this run's output should go.
    ///
    /// A WinExe owns no console, so when one launched us we have to borrow it or every line the CLI
    /// prints goes nowhere. But borrowing is exactly wrong when the caller redirected us: `diag >
    /// file.txt` already hands us a valid handle, and attaching a console replaces it, which is why
    /// redirection produced an empty file while piping happened to work.
    ///
    /// So the redirection test has to come first, before anything can change the handles under it.
    /// Either way the streams are rebound afterwards, because .NET caches a writer on first use and
    /// the one it cached may predate the console existing at all.
    /// </summary>
    private static void AttachToOutput()
    {
        bool redirected = Console.IsOutputRedirected;
        if (!redirected) AttachConsole(AttachParentProcess);

        Rebind(Console.OpenStandardOutput, Console.SetOut);
        Rebind(Console.OpenStandardError, Console.SetError);

        static void Rebind(Func<Stream> open, Action<TextWriter> set)
        {
            try
            {
                var s = open();
                if (s != Stream.Null) set(new StreamWriter(s) { AutoFlush = true });
            }
            catch (IOException) { }   // no console and no redirection: nothing to write to, and that is fine
        }
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
