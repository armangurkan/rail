use clap::{Parser, Subcommand, ValueEnum};

/// Modern email. Native to your terminal.
#[derive(Debug, Parser)]
#[command(name = "rail", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage accounts.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Check config, store, secrets and accounts.
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Log in to a provider and add the account to config.toml.
    Add(AddArgs),
    /// List configured accounts and whether tokens are stored.
    List,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Provider.
    pub kind: KindArg,
    /// Email address of the account.
    #[arg(long)]
    pub address: String,
    /// OAuth client id from Google Cloud or Azure.
    #[arg(long)]
    pub client_id: String,
    /// OAuth client secret (Google desktop clients have one; Azure public clients do not).
    #[arg(long)]
    pub client_secret: Option<String>,
    /// Microsoft tenant (default: common).
    #[arg(long)]
    pub tenant: Option<String>,
    /// Short name used in the UI and config (default: derived from the address).
    #[arg(long)]
    pub name: Option<String>,
    /// Account colour in the UI.
    #[arg(long)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum KindArg {
    Gmail,
    M365,
}
