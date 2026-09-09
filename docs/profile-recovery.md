# Profile trials and recovery

Apply, profile-slot loads and startup configuration apply immediately without a
popup or countdown. Successful driver writes become the recovery baseline; this
does not prove stability. An apply error restores the previous successful profile.
Before the first successful apply, rollback uses the capability-derived stock profile.

The per-GPU journal is flushed to disk before writes. An interrupted trial is
reverted on the next launch and startup application is skipped on that launch.
Failed rollback retains the journal for another recovery attempt. This is not a
separate watchdog: a hung driver or frozen OS may require reboot/relaunch. Recovery
is not an automatic stability test or protection from damage.

Enabling or changing Startup requires a successful apply of the saved slot. Startup
uses a frozen copy, not the editable slot file. Saving a slot or fan changes does
not update that copy: toggle Startup off/on to update it. Legacy startup entries
without a saved copy are skipped until refreshed in the UI.

Zero RPM is in Fan Control. Its Apply writes only fan mode, speed/curve and Zero
RPM; it cannot apply pending clock or voltage changes. Fan-only and individual
advanced XOC lever writes are not full-profile trials.

Core tests exercise recovery with mock writes, including restart recovery,
rollback failures, confirmation rejection, isolated GPU journals and immutable
startup copies. They never tune real hardware.
