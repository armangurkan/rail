use clap::{Parser, Subcommand, ValueEnum};

/// Modern email. Native to your terminal.
#[derive(Debug, Parser)]
#[command(name = "rail", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Test only: panic inside the alternate screen to check the terminal is restored.
    #[arg(long, hide = true)]
    pub panic_test: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add an account with the interactive setup wizard.
    Setup,
    /// Manage accounts.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Check config, store, secrets, accounts and the daemon.
    Doctor,
    /// Serve Rail's tools to an AI coding agent over MCP on stdin and stdout.
    Mcp,
    /// The background daemon that runs every operation. Started on demand.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Run the daemon in the foreground (Rail starts it this way when needed).
    Run,
    /// Say whether the daemon is running, and what it is doing.
    Status,
    /// Ask the daemon to exit once running saves finish.
    Stop,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Add an account. With no arguments, or only a provider, the setup wizard opens.
    Add(AddArgs),
    /// Log in again to an account already in config.toml and replace its tokens.
    Login {
        /// Account name or email address.
        account: String,
    },
    /// List configured accounts and whether tokens are stored.
    List,
    /// Remove an account's config entry and stored tokens. Its mail stays in the store.
    /// Asks first, so it runs only on an interactive terminal.
    Remove {
        /// Account name or email address.
        account: String,
    },
}

/// The flag form needs a provider, `--address` and `--client-id`; every other flag needs
/// `--address`. Without `--address` the wizard opens. The combinations are checked in
/// `cmd::account::new_account_from_args`.
#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Provider. Alone, it opens the setup wizard for that provider.
    pub kind: Option<KindArg>,
    /// Email address of the account.
    #[arg(long)]
    pub address: Option<String>,
    /// OAuth client id from Google Cloud or Azure.
    #[arg(long)]
    pub client_id: Option<String>,
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
