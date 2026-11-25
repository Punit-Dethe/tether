use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use tether::cli::Cli;
use tether::commands;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dir = match cli.repo.clone() {
        Some(dir) => dir,
        None => std::env::current_dir().unwrap_or_else(|_| ".".into()),
    };
    let report = commands::run(&cli.command, &dir);
    let mut out = std::io::stdout().lock();
    // A closed pipe (`tether status | head`) is not worth a panic.
    let _ = out.write_all(report.render(cli.json).as_bytes());
    ExitCode::from(report.exit_code())
}
