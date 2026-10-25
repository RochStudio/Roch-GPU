//! Native build and AMD diagnostic tools. No shell or scripting runtime is invoked.
mod amd;
mod build;
mod pe;

type Result<T> = std::result::Result<T, String>;

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            println!(
                "Roch GPU developer tools\n\n  cargo xtask build [--out-dir PATH]\n  cargo xtask amd <probe|exports|features|write-check|telemetry> [OPTIONS]\n\nAMD options: --mock, --adapter INDEX, --out FILE\nwrite-check additionally requires --apply on real hardware; --offset MHz defaults to 200.\nAll other AMD commands are read-only. --mock never loads a driver."
            );
            Ok(())
        }
        Some("build") => build::run(&args[1..]),
        Some("amd") => amd::run(&args[1..]),
        Some(other) => Err(format!("Unknown command '{other}'. Use cargo xtask --help.")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_commands_fail_before_doing_work() {
        assert!(super::run(vec!["invalid".into()]).is_err());
        assert!(super::run(vec!["amd".into(), "invalid".into()]).is_err());
        assert!(super::run(vec!["build".into(), "--invalid".into()]).is_err());
    }
}
