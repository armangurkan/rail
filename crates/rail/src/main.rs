mod cli;

use clap::Parser;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let _cli = cli::Cli::parse();
    eprintln!("rail: the TUI is not built yet; run `rail --help` for available commands");
    std::process::exit(2);
}
