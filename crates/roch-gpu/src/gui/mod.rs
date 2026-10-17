//! The window half. Placeholder until the egui GUI lands: the GUI port replaces this module and
//! keeps the `run` signature (`args` = the window's own flags, e.g. `--minimized`,
//! `--apply-profile <name>`; returns the process exit code). It should call
//! `crate::single_instance::claim()` / `listen_for_show_requests` itself, as the WPF App did.

/// Run the window. Not implemented yet.
pub fn run(_args: &[String]) -> i32 {
    eprintln!("GUI not yet implemented");
    1
}
