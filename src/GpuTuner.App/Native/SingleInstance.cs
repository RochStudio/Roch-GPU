using System;
using System.Threading;

namespace GpuTuner.App.Native;

/// <summary>
/// One window at a time.
///
/// This did not matter while the logon task applied a profile and quit: nothing was left running to
/// collide with. A resident startup run changes that — the app is already sitting in the tray when
/// you double-click the desktop icon, and without a guard you get two elevated processes writing
/// clocks and voltages to the same card, each unaware of the other's applies.
///
/// The check runs before the elevation prompt, so a redundant launch costs no UAC dialog.
///
/// Two integrity levels are in play: the logon instance is elevated because the task asks for the
/// highest privileges, while a launch from the shell starts medium and only elevates afterwards. A
/// medium process cannot open a high-integrity object for modify, so "access denied" here means the
/// mutex exists and belongs to a more privileged copy of us — which is a yes, not an error.
/// </summary>
public static class SingleInstance
{
    private const string MutexName = @"Global\RochGPU.SingleInstance";
    private const string ShowEventName = @"Global\RochGPU.Show";

    private static Mutex? _held;
    private static EventWaitHandle? _show;

    /// <summary>True when this process is the first one. Keeps the handle for the process's life.</summary>
    public static bool Claim()
    {
        try
        {
            _held = new Mutex(initiallyOwned: true, MutexName, out bool first);
            if (!first) { _held.Dispose(); _held = null; }
            return first;
        }
        catch (UnauthorizedAccessException) { return false; }   // an elevated copy holds it
    }

    /// <summary>Is one already running? Asked before elevating, so it must survive being refused.</summary>
    public static bool AlreadyRunning()
    {
        try { using var m = Mutex.OpenExisting(MutexName); return true; }
        catch (WaitHandleCannotBeOpenedException) { return false; }
        catch (UnauthorizedAccessException) { return true; }
    }

    /// <summary>
    /// Ask the running copy to show itself. Best effort: if it is elevated and this process is not,
    /// the event cannot be opened for modify, and the second copy simply steps aside rather than
    /// fighting for the card. Stepping aside is the part that matters.
    /// </summary>
    public static void AskExistingToShow()
    {
        try { EventWaitHandle.OpenExisting(ShowEventName).Set(); }
        catch (WaitHandleCannotBeOpenedException) { }
        catch (UnauthorizedAccessException) { }
    }

    /// <summary>Listen for a later launch asking us to come back from the tray.</summary>
    public static void ListenForShowRequests(Action show)
    {
        try { _show = new EventWaitHandle(false, EventResetMode.AutoReset, ShowEventName); }
        catch (UnauthorizedAccessException) { return; }

        var thread = new Thread(() =>
        {
            while (_show != null && _show.WaitOne()) show();
        })
        { IsBackground = true, Name = "RochGPU show listener" };
        thread.Start();
    }
}
