mod browser;
mod cli;
mod cmd;

use clap::Parser;
use cli::{AccountCommand, Cli, Command};
use rail_core::paths::Paths;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    match cli.command {
        None => {
            eprintln!("rail: the TUI is not built yet; run `rail --help` for available commands");
            std::process::exit(2);
        }
        Some(Command::Account {
            command: AccountCommand::Add(args),
        }) => cmd::account::add(&paths, args).await,
        Some(Command::Account {
            command: AccountCommand::List,
        }) => cmd::account::list(&paths),
        Some(Command::Doctor) => {
            eprintln!("rail doctor: not implemented yet");
            std::process::exit(2);
        }
    }
}
