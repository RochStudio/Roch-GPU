//! One executable, two front ends (C# `EntryPoint`).
//!
//! A verb ("info", "apply", ...) runs the command line; no arguments, or the window's own --flags,
//! runs the window. The two never collide because CLI verbs never begin with a dash and every GUI
//! flag does — which matters, since the logon task launches the window with --minimized and
//! --apply-profile.

// A windows-subsystem exe, like the WPF WinExe; the CLI borrows the caller's console instead.
// (Not for the test harness, whose output cargo captures.)
#![cfg_attr(not(test), windows_subsystem = "windows")]

#[cfg(test)]
mod audit_dump;
mod backends;
mod cli;
mod console;
mod elevation;
mod gui;
mod single_instance;

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let code = entry(&args);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code);
}

fn entry(args: &[String]) -> i32 {
    if wants_command_line(args) {
        let _console = console::attach_to_output(); // restores the code page on the way out
        return cli::run(args);
    }

    // Before the prompt, not after: a second launch while the logon copy sits in the tray should
    // cost nothing at all, and asking for elevation first would cost a UAC dialog to then do
    // nothing with.
    if single_instance::already_running() {
        single_instance::ask_existing_to_show();
        return 0;
    }

    if wants_elevation(args) && !elevation::is_elevated() && elevation::relaunch_elevated(args) {
        return 0;
    }

    gui::run(args)
}

/// The window elevates because it writes clocks. The simulated GPU (`--mock`) writes nothing, so
/// it runs at whatever level it was started with: no UAC prompt for a UI demo, and the GUI can be
/// run and tested from an ordinary shell. (Deviation from the WPF build, which always elevated.)
fn wants_elevation(args: &[String]) -> bool {
    !args.iter().any(|a| a == "--mock")
}

/// Help is the one word both halves answer to; the CLI's version is the useful one.
fn wants_command_line(args: &[String]) -> bool {
    match args.first() {
        Some(first) => !first.starts_with('-') || first == "-h" || first == "--help",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn verbs_run_the_cli_and_flags_run_the_window() {
        assert!(wants_command_line(&v(&["info"])));
        assert!(wants_command_line(&v(&["apply", "--core", "15"])));
        assert!(wants_command_line(&v(&["help"])));
        assert!(wants_command_line(&v(&["-h"])));
        assert!(wants_command_line(&v(&["--help"])));
        assert!(!wants_command_line(&v(&[])));
        assert!(!wants_command_line(&v(&["--minimized"])));
        assert!(!wants_command_line(&v(&["--apply-profile", "Slot 1", "--minimized"])));
        assert!(!wants_command_line(&v(&["--mock"])));
    }

    #[test]
    fn only_the_simulated_gpu_skips_elevation() {
        assert!(wants_elevation(&v(&[])));
        assert!(wants_elevation(&v(&["--apply-profile", "Slot 1", "--minimized"])));
        assert!(!wants_elevation(&v(&["--mock"])));
        assert!(!wants_elevation(&v(&["--minimized", "--mock"])));
    }
}
