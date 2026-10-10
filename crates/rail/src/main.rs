mod browser;
mod cli;
mod cmd;
mod logging;

use std::process::ExitCode;

use clap::Parser;
use cli::{AccountCommand, Cli, Command};
use rail_core::paths::Paths;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = color_eyre::install() {
        eprintln!("rail: {e}");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the command. Returning (rather than exiting) lets the log guard drop
/// and flush before the process ends.
async fn run(cli: Cli) -> color_eyre::Result<ExitCode> {
    let paths = Paths::from_env()?;
    let _log_guard = logging::init(&paths);
    match cli.command {
        None => {
            eprintln!("rail: the TUI is not built yet; run `rail --help` for available commands");
            Ok(ExitCode::from(2))
        }
        Some(Command::Account {
            command: AccountCommand::Add(args),
        }) => cmd::account::add(&paths, args)
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Account {
            command: AccountCommand::List,
        }) => cmd::account::list(&paths).map(|()| ExitCode::SUCCESS),
        Some(Command::Doctor) => {
            let report = cmd::doctor::gather(&paths);
            print!("{}", cmd::doctor::render(&report));
            Ok(if report.is_healthy() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
    }
}
