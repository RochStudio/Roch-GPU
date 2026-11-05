//! `rochgpu setup`: the one step on Linux that needs root.
//!
//! The tuning files belong to root, and running the window as root is more trouble on Wayland than the
//! permission is worth. So, the way OpenRGB does for its devices, a udev rule hands those files to the
//! person who ran the setup, at every boot, and everything else runs as that person. Windows asks for
//! the same permission with a UAC prompt at each launch; here it is asked once.
//!
//! Two kernel settings live beside it, both opt-in because both change how the driver behaves:
//! `--overdrive` (the clock, voltage and fan controls exist only with this on) and `--keep-awake` (for
//! a card that does not come back from the kernel's idle power-down).

use std::io::Write;
use std::path::Path;
use std::process::Command;

use roch_amdgpu::backend::OVERDRIVE_BIT;
use roch_amdgpu::sysfs;
use roch_core::services::profile_store::storage_root;

use crate::cli::Opts;

const RULES_PATH: &str = "/etc/udev/rules.d/60-roch-gpu.rules";
const MODPROBE_PATH: &str = "/etc/modprobe.d/roch-gpu.conf";
const DRM_ROOT: &str = "/sys/class/drm";
const FEATURE_MASK: &str = "/sys/module/amdgpu/parameters/ppfeaturemask";
const RUNPM: &str = "/sys/module/amdgpu/parameters/runpm";

macro_rules! say {
    ($w:expr) => { let _ = writeln!($w); };
    ($w:expr, $($arg:tt)*) => { let _ = writeln!($w, $($arg)*); };
}

/// A name that is safe inside the shell command of a udev rule, which runs as root. Deliberately
/// narrower than what the system allows: this is the only thing from outside that reaches that line.
pub fn is_safe_user_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The rule file. One rule for the card, one for its hwmon in case that registers later. `%p` is the
/// device path from the kernel, `$$` a literal `$`, and every path is the kernel's own, so nothing a
/// user controls is run or followed. Quotes are left off the shell's variables because a udev value
/// cannot hold a double quote, and these file names have no spaces.
pub fn udev_rules(user: &str) -> String {
    let device_files = sysfs::OWNED_FILES.join(" ");
    let hwmon_files = sysfs::OWNED_HWMON_FILES.join(" ");
    let hwmon_globs: Vec<String> = sysfs::OWNED_HWMON_FILES.iter().map(|f| format!("hwmon/hwmon*/{f}")).collect();
    let hwmon_globs = hwmon_globs.join(" ");
    let chown = |files: &str| format!("for f in {files}; do [ -f $$f ] && /bin/chown {user} $$f; done; true");
    format!(
        "# Written by `rochgpu setup`. Lets {user} change AMD GPU clocks, voltage, power and fans without root.\n\
         # Delete this file, or run `sudo rochgpu setup --undo`, to take that back.\n\
         ACTION==\"add|change\", SUBSYSTEM==\"drm\", KERNEL==\"card[0-9]*\", ENV{{DEVTYPE}}==\"drm_minor\", \
         DRIVERS==\"amdgpu\", RUN+=\"/bin/sh -c 'cd /sys%p/device && {}'\"\n\
         ACTION==\"add\", SUBSYSTEM==\"hwmon\", ATTR{{name}}==\"amdgpu\", RUN+=\"/bin/sh -c 'cd /sys%p && {}'\"\n",
        chown(&format!("{device_files} {hwmon_globs}")),
        chown(&hwmon_files),
    )
}

/// `options amdgpu ...`, keeping whichever option this run is not changing.
pub fn module_options(existing: &str, feature_mask: Option<u64>, keep_awake: bool) -> String {
    let current = |key: &str| {
        existing
            .split_whitespace()
            .find_map(|w| w.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(str::to_string)
    };
    let mask = feature_mask.map(|m| format!("0x{m:08x}")).or_else(|| current("ppfeaturemask"));
    let runpm = if keep_awake { Some("0".to_string()) } else { current("runpm") };
    let mut line = String::from("options amdgpu");
    if let Some(mask) = mask {
        line.push_str(&format!(" ppfeaturemask={mask}"));
    }
    if let Some(runpm) = runpm {
        line.push_str(&format!(" runpm={runpm}"));
    }
    format!("# Written by `rochgpu setup`. Remove with `sudo rochgpu setup --undo`.\n{line}\n")
}

fn parse_hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// Which command puts `/etc/modprobe.d` into this distribution's boot image. amdgpu usually loads from
/// there, before the root filesystem, so a setting only in `/etc` would not reach it.
fn rebuild_command() -> Option<&'static str> {
    let on_path = |tool: &str| {
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(tool).is_file()))
    };
    // limine-mkinitcpio first: where it exists, plain mkinitcpio leaves the boot entries out of date.
    [
        ("limine-mkinitcpio", "sudo limine-mkinitcpio"),
        ("mkinitcpio", "sudo mkinitcpio -P"),
        ("update-initramfs", "sudo update-initramfs -u"),
        ("dracut", "sudo dracut --force"),
    ]
    .into_iter()
    .find(|(tool, _)| on_path(tool))
    .map(|(_, command)| command)
}

fn say_rebuild(out: &mut dyn Write) {
    say!(out);
    say!(out, "This takes effect after a reboot. First put it in the boot image:");
    match rebuild_command() {
        Some(command) => {
            say!(out, "  {command}");
        }
        None => {
            say!(out, "  (rebuild your initramfs the way your distribution does)");
        }
    }
    say!(out, "  sudo reboot");
}

fn status(out: &mut dyn Write) -> i32 {
    match std::fs::read_to_string(RULES_PATH) {
        Ok(rules) => {
            let who = rules.split_whitespace().skip_while(|w| *w != "Lets").nth(1).unwrap_or("?");
            say!(out, "Permission: {who} may change the card ({RULES_PATH})");
        }
        Err(_) => {
            say!(out, "Permission: not set up. Changes need sudo until `{}` has been run.", sysfs::setup_command(""));
        }
    }
    for dir in sysfs::amdgpu_device_dirs(Path::new(DRM_ROOT)) {
        let files = sysfs::owned_files(&dir);
        let writable = files.iter().filter(|f| sysfs::can_write(f)).count();
        say!(out, "  {}: this user can write {writable} of {} tuning files", dir.display(), files.len());
    }
    let mask = sysfs::read_text(Path::new(FEATURE_MASK));
    let on = mask.as_deref().and_then(parse_hex).is_some_and(|m| m & OVERDRIVE_BIT != 0);
    say!(out, "Overdrive : {} in the running kernel (ppfeaturemask {})", if on { "on" } else { "off" }, {
        mask.as_deref().unwrap_or("n/a")
    });
    say!(
        out,
        "Keep awake: runpm is {} in the running kernel",
        sysfs::read_text(Path::new(RUNPM)).as_deref().unwrap_or("n/a")
    );
    if let Ok(options) = std::fs::read_to_string(MODPROBE_PATH) {
        let line = options.lines().find(|l| l.starts_with("options")).unwrap_or("");
        say!(out, "Configured: {line} ({MODPROBE_PATH})");
    }
    0
}

fn require_root(err: &mut dyn Write, flags: &str) -> bool {
    let is_root = std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0);
    if !is_root {
        say!(err, "This changes system files, so it needs root. Run: {}", sysfs::setup_command(flags));
    }
    is_root
}

/// Hand the card's tuning files to `uid` (or back to root) now, so nothing waits for a reboot.
fn chown_now(uid: u32) -> usize {
    let mut changed = 0;
    for dir in sysfs::amdgpu_device_dirs(Path::new(DRM_ROOT)) {
        for file in sysfs::owned_files(&dir) {
            if std::os::unix::fs::chown(&file, Some(uid), None).is_ok() {
                changed += 1;
            }
        }
    }
    changed
}

fn reload_udev() {
    let _ = Command::new("udevadm").args(["control", "--reload"]).status();
}

fn allow_user(o: &Opts, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let invoker = storage_root::invoker();
    let name = o
        .get("user")
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| invoker.as_ref().map(|i| i.name.clone()));
    let Some(name) = name else {
        say!(err, "Could not tell who this is for. Run it with sudo from your own account, or name them: --user NAME");
        return 2;
    };
    if !is_safe_user_name(&name) {
        say!(err, "'{name}' is not a user name this can safely put in a udev rule.");
        return 2;
    }
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let Some(uid) = passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name.as_str())).then(|| fields.nth(1)?.parse::<u32>().ok())?
    }) else {
        say!(err, "There is no user called '{name}'.");
        return 2;
    };
    if let Err(e) = std::fs::write(RULES_PATH, udev_rules(&name)) {
        say!(err, "Could not write {RULES_PATH}: {e}");
        return 1;
    }
    reload_udev();
    let changed = chown_now(uid);
    say!(
        out,
        "{name} can now change the card without sudo: {changed} tuning files handed over, and {RULES_PATH} does the same at every boot."
    );
    0
}

/// Write the module options, returning whether anything was written.
fn set_module_options(overdrive: bool, keep_awake: bool, out: &mut dyn Write, err: &mut dyn Write) -> bool {
    let existing = std::fs::read_to_string(MODPROBE_PATH).unwrap_or_default();
    let mask = if overdrive {
        let Some(current) = sysfs::read_text(Path::new(FEATURE_MASK)).as_deref().and_then(parse_hex) else {
            say!(err, "Could not read amdgpu's current feature mask from {FEATURE_MASK}. Is the driver loaded?");
            return false;
        };
        Some(current | OVERDRIVE_BIT)
    } else {
        None
    };
    if let Err(e) = std::fs::write(MODPROBE_PATH, module_options(&existing, mask, keep_awake)) {
        say!(err, "Could not write {MODPROBE_PATH}: {e}");
        return false;
    }
    if overdrive {
        say!(out, "Overdrive will be on from the next boot. Running a card outside its stock clocks and voltage can");
        say!(out, "crash the machine and in the extreme damage hardware, and the kernel marks itself as tainted while");
        say!(out, "this is on, so mention it in any bug report. Remove it with `{}`.", sysfs::setup_command(" --undo"));
    }
    if keep_awake {
        say!(out, "From the next boot the kernel will not power idle AMD cards down. A second card then keeps");
        say!(out, "drawing its idle power (around 15 W on an RX 7900 XT) instead of switching off.");
    }
    true
}

fn undo(out: &mut dyn Write) -> i32 {
    let mut changed_boot = false;
    if std::fs::remove_file(RULES_PATH).is_ok() {
        reload_udev();
        chown_now(0);
        say!(out, "Removed {RULES_PATH}. The tuning files belong to root again.");
    }
    if std::fs::remove_file(MODPROBE_PATH).is_ok() {
        say!(out, "Removed {MODPROBE_PATH}.");
        changed_boot = true;
    }
    if changed_boot {
        say_rebuild(out);
    }
    0
}

/// What a run would write, for reading before handing it root.
fn show(o: &Opts, out: &mut dyn Write) -> i32 {
    let name = o.get("user").filter(|n| !n.is_empty()).map(str::to_string);
    let name = name
        .or_else(|| storage_root::invoker().map(|i| i.name))
        .or_else(|| std::env::var("USER").ok())
        .filter(|n| is_safe_user_name(n))
        .unwrap_or_else(|| "USER".into());
    say!(out, "# {RULES_PATH}");
    let _ = out.write_all(udev_rules(&name).as_bytes());
    let mask = sysfs::read_text(Path::new(FEATURE_MASK)).as_deref().and_then(parse_hex).unwrap_or(0xffff_ffff);
    say!(out);
    say!(out, "# {MODPROBE_PATH}, with --overdrive and --keep-awake");
    let _ = out.write_all(module_options("", Some(mask | OVERDRIVE_BIT), true).as_bytes());
    0
}

pub fn run(o: &Opts, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if o.contains("status") {
        return status(out);
    }
    if o.contains("show") {
        return show(o, out);
    }
    if o.contains("undo") {
        return if require_root(err, " --undo") { undo(out) } else { 1 };
    }
    let (overdrive, keep_awake) = (o.contains("overdrive"), o.contains("keep-awake"));
    let flags =
        format!("{}{}", if overdrive { " --overdrive" } else { "" }, if keep_awake { " --keep-awake" } else { "" });
    if !require_root(err, &flags) {
        return 1;
    }
    let code = allow_user(o, out, err);
    if code != 0 || !(overdrive || keep_awake) {
        return code;
    }
    if !set_module_options(overdrive, keep_awake, out, err) {
        return 1;
    }
    say_rebuild(out);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_reach_the_root_shell() {
        for ok in ["local", "mateo", "a.b-c_d", "user1"] {
            assert!(is_safe_user_name(ok), "{ok}");
        }
        for bad in ["", "-rf", "a b", "a;reboot", "$(reboot)", "a'b", "a\"b", "a`b", "a/b", "a\nb", "é"] {
            assert!(!is_safe_user_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_rules_name_the_user_and_every_file_the_backend_writes() {
        let rules = udev_rules("local");
        assert_eq!(rules.lines().filter(|l| !l.starts_with('#')).count(), 2, "{rules}");
        assert!(rules.contains("/bin/chown local $$f"));
        for file in [
            "pp_od_clk_voltage",
            "power_dpm_force_performance_level",
            "gpu_od/fan_ctrl/fan_curve",
            "gpu_od/fan_ctrl/fan_zero_rpm_enable",
            "hwmon/hwmon*/power1_cap",
            "hwmon/hwmon*/pwm1",
            "hwmon/hwmon*/pwm1_enable",
        ] {
            assert!(rules.contains(file), "{file} missing from {rules}");
        }
        let first_rule = rules.lines().find(|l| l.starts_with("ACTION")).unwrap();
        assert!(first_rule.contains("DRIVERS==\"amdgpu\""), "must not touch another vendor's card");
        assert!(first_rule.contains("ENV{DEVTYPE}==\"drm_minor\""), "card1-DP-1 also matches card[0-9]*");
    }

    #[test]
    fn module_options_keep_what_this_run_is_not_changing() {
        let body = |text: &str| text.lines().last().unwrap().to_string();
        assert_eq!(body(&module_options("", Some(0xfff7_ffff), false)), "options amdgpu ppfeaturemask=0xfff7ffff");
        assert_eq!(body(&module_options("", None, true)), "options amdgpu runpm=0");
        let existing = module_options("", Some(0xfff7_ffff), false);
        assert_eq!(body(&module_options(&existing, None, true)), "options amdgpu ppfeaturemask=0xfff7ffff runpm=0");
        let both = module_options(&existing, None, true);
        assert_eq!(
            body(&module_options(&both, Some(0xffff_ffff), false)),
            "options amdgpu ppfeaturemask=0xffffffff runpm=0"
        );
    }

    #[test]
    fn the_overdrive_bit_is_added_to_what_the_kernel_has_now() {
        // This card's real mask: everything else in it stays as the driver chose.
        let mask = parse_hex("0xfff7bfff").unwrap();
        assert_eq!(mask & OVERDRIVE_BIT, 0);
        assert_eq!(mask | OVERDRIVE_BIT, 0xfff7_ffff);
    }
}
