# Roch GPU on Linux

The same app as on Windows, the window and the command line, for **AMD cards using the kernel's `amdgpu` driver**. Wayland and X11 both work.

> Writing clocks and voltages to a GPU can crash the machine, corrupt work in progress, and in the extreme damage hardware.

NVIDIA and Intel Arc are Windows only for now: their backends are built on vendor DLLs that do not exist on Linux. On a machine with only those cards, the app names what it found and says so.

Verified on two RX 7900 XTs with kernel 7.2: the [technical guide](development.md#verification-status) has what was run on hardware and what was not. Other cards follow from what the driver reports, but have not been tried.

## Install

You need [Rust stable](https://rustup.rs). No other development packages are required.

```bash
cargo xtask install
```

This builds a release binary and installs it for your user, with no root needed:

| File | What |
|---|---|
| `~/.local/bin/rochgpu` | The program. No arguments opens the window; a verb runs the command line |
| `~/.local/share/applications/RochGPU.desktop` | The launcher entry |
| `~/.local/share/icons/hicolor/256x256/apps/RochGPU.png` | The icon |

Use `--prefix PATH` to install somewhere else, and `cargo xtask uninstall` to remove them. The binary links nothing beyond libc, so the command line also runs on a machine with no desktop.

Reading sensors works straight away:

```bash
rochgpu info
```

## Let it change the card

The tuning files belong to root. Run this once, and they are handed to you at every boot, so neither the window nor the command line needs root afterwards:

```bash
sudo ~/.local/bin/rochgpu setup
```

(`sudo rochgpu` usually fails on a home install, because sudo only searches its own short path.)

It writes one udev rule, `/etc/udev/rules.d/60-roch-gpu.rules`, and changes the owner of the files for the card now. `rochgpu setup --show` prints exactly what it would write. Use `--user NAME` to set it up for someone else.

Without it, reading still works, a change is refused with a message saying this, and `sudo rochgpu apply ...` works for one command at a time.

## Turn on the clock, voltage and fan controls

By default the kernel only offers the power limit. Clock, voltage and fan controls exist only once the driver's overdrive feature is on:

```bash
sudo ~/.local/bin/rochgpu setup --overdrive
```

That writes `/etc/modprobe.d/roch-gpu.conf`, and then tells you the command that puts it in your boot image (for example `sudo limine-mkinitcpio` on Omarchy, `sudo mkinitcpio -P` on Arch, `sudo update-initramfs -u` on Debian and Ubuntu, `sudo dracut --force` on Fedora). Run that, then reboot.

While overdrive is on the kernel marks itself as tainted, so mention it in any kernel bug report. `rochgpu setup --status` shows what the running kernel has, and `sudo ~/.local/bin/rochgpu setup --undo` removes everything the setup added.

## A card that says "Asleep"

The kernel powers an idle card with no display attached down to D3cold. Roch GPU never wakes a sleeping card: it shows the card, reports it as asleep, and leaves its controls off until something else wakes it.

The reason is a real hazard. On some machines a card fails to come back from D3cold, and everything that touches it then blocks in the kernel, in a state that cannot be killed. If you have a second card, check for it with:

```bash
journalctl -k | grep -E 'rlc autoload|resume of IP block'
```

Repeating errors there mean the card is unusable until the next boot. Keeping the kernel from powering cards down avoids the failed wake, at the cost of the card's idle power (around 15 W on an RX 7900 XT). On the pair of RX 7900 XTs this was developed on, the second card failed its resume about 14 times a minute from boot; with this set there were no failures and both cards read normally:

```bash
sudo ~/.local/bin/rochgpu setup --keep-awake
```

As with overdrive, rebuild the boot image and reboot. `--overdrive` and `--keep-awake` can be given together.

## What differs from Windows

The controls, ranges and units are the same wherever the kernel and Wayland allow it, so a memory clock of 2500 MHz means the same thing on both. These differences remain because Linux offers no equivalent:

| | Windows | Linux |
|---|---|---|
| Asking for permission | UAC prompt at each launch | `rochgpu setup`, once |
| RDNA 3 voltage control | Maximum voltage | Offset to the whole curve, the only voltage control amdgpu has |
| Temperature limit | Where the driver offers it | Not offered: amdgpu's thermal limits are read-only |
| Memory timing | Where the driver offers it | Not offered: no such control in amdgpu |
| Tray icon | Yes | No: Wayland gives a program no way to hide its own window. Minimizing works as your desktop does, and on Hyprland see below |
| Child windows with the main window out of sight | Keep updating | Keep updating, unless a fullscreen window covers the main window: see below |
| Startup profile | Stays resident in the tray | Applies and exits. The firmware keeps running a hardware fan curve, but a software curve on an older card needs the window open |
| Window position, opacity | Remembered, adjustable | Up to your compositor |

**Minimizing on Hyprland.** Hyprland has no tray and no minimized state, so its scratchpad workspace stands in for them. The minimize button does what going to the tray does on Windows: it closes Telemetry and the other child windows, stops reading the card unless a fan curve needs a temperature, and moves the main window out of sight. To bring it back, run `rochgpu` again, which puts it on the workspace you are on, or press `Super+S` on Omarchy, which opens the scratchpad over it (elsewhere, bind `togglespecialworkspace scratchpad` yourself). Either way, a Telemetry or Fan window that was open reopens. `Super+S` only shows and hides, so whatever is open keeps updating out of sight until you press the minimize button again. `rochgpu --minimized` starts out hidden, and if closing the window would stop a software fan curve, it offers to hide instead, as Windows offers the tray.

The window does not need to stay open for most things: clocks, voltage, the power limit and a hardware fan curve stay on the card after it closes, until a reboot.

**Windows out of sight.** Every child window draws during the main window's frame, and a Wayland compositor stops giving frames to a window nobody can see. On Hyprland, Roch GPU asks for them anyway (`render_unfocused`), so Telemetry keeps updating with the main window on another workspace. That does not reach a fullscreen window on top of the main window, which includes making a child window itself fullscreen: the child windows stop drawing until the main window is uncovered. So with a game on one monitor and Telemetry on another, keep the main window off the game's workspace. Telemetry's figures keep counting the whole time, so its minimums and maximums are right when it draws again. Other desktops have not been tried and have no such setting, so expect the same there whenever the main window is out of sight.

**Fan control.** RDNA 3 and later cards run their fans in firmware, and those fans are only driven through the firmware's `fan_curve`, which needs overdrive on. The kernel still lists `pwm1` as writable on those cards, but taking manual control through it can leave the fan stopped on a hot card, so Roch GPU does not use it. Older cards are driven through `pwm1`, with the curve stepped by the running window.

As on Windows, zero RPM has its own control. While it is on, the firmware holds the fan still below its stop temperature whatever fixed speed was asked for, so turn it off in the Fan window for a fan that always spins.

**Memory clock.** It has the same range as on Windows, which starts at the card's stock clock: 2500 to 3000 MHz on an RX 7900 XT. The kernel counts the memory controller's clock, half of that on GDDR6, and would accept far lower values, but Roch GPU does not go there. Capping an RX 7900 XT at 800 MHz on the kernel's scale while it ran a compute job hung the card's power management firmware and froze the machine: `SMU: No response` in the kernel log, then nothing until a hard reset. Lowering it gains nothing anyway, since the card already drops its memory clock when idle. Raising it by 20 MHz on an idle RX 7900 XT was accepted and read back. As on Windows, every Apply sends the memory clock, even when only something else changed. Whether a raised limit gives a faster clock under load, and sending it to a busy card at any value, have not been tried on hardware, so try an Apply on an idle card first.

**Limit reasons.** The firmware's throttling bits are not taken at face value. An RX 7900 XT raises its hot spot bit at idle, 60 °C under its limit, and its power bit in the burst of activity after boot, at 40 % of its power cap. So a thermal bit counts only within 20 °C of a sensor's critical temperature, and a power bit only at 75 % of the cap or more.

**Stock values.** The driver does not publish a card's stock clocks. Roch GPU records them the first time it reads the card after a boot, so if another tool has already changed them by then, what it calls stock is what that tool set. They can also shift from one boot to the next: 2815, 2810 and 2770 MHz on one card here. Reset always uses the driver's own restore, so it is right either way.

## Command line

```text
rochgpu info [--gpu 0]        cards, ranges, current settings, one sample, and what is holding controls back
rochgpu sensors [--gpu 0]     every sensor
rochgpu monitor [--gpu 0] [--interval 1000]
rochgpu apply [--gpu 0] [--core 2500] [--core-min 500] [--mem 2600] [--power -5] [--uv -50] [--fan 60|auto]
rochgpu apply-profile <name> [--gpu 0]
rochgpu reset [--gpu 0]
rochgpu diag                  everything the driver reports, for bug reports
rochgpu startup --enable <profile> | --disable | --status
rochgpu setup [--overdrive] [--keep-awake] [--user NAME] | --status | --show | --undo
```

On RDNA 2 and 3, `--core` is the maximum core clock and `--core-min` the minimum, both in MHz. `--mem` is the maximum memory clock in MHz, in the same units as Windows. `--power` is a percentage above or below the card's default. `--uv` is a negative voltage offset in mV. Run `rochgpu info` for your card's ranges.

GPU 0 is the card driving your display, then the rest in PCI order.

Settings and profiles are in `~/.config/Roch GPU`, including under sudo, which writes them as you rather than as root.
