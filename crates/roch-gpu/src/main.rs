//! One executable, two front ends (C# `EntryPoint`).
//!
//! A verb ("info", "apply", ...) runs the command line; no arguments, or the window's own --flags,
//! runs the window. The two never collide because CLI verbs never begin with a dash and every GUI
//! flag does — which matters, since the logon task launches the window with --minimized and
//! --apply-profile.

// A windows-subsystem exe, like the WPF WinExe; the CLI borrows the caller's console instead.
// (Not for the test harness, whose output cargo captures.)
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod backends;
mod cli;
mod console;
mod elevation;
mod gui;
// `claim` and `listen_for_show_requests` belong to the window half; the GUI port calls them.
#[allow(dead_code)]
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

    if !elevation::is_elevated() && elevation::relaunch_elevated(args) {
        return 0;
    }

    gui::run(args)
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
}
