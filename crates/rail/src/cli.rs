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
    /// Choose where tokens are kept.
    Secrets {
        #[command(subcommand)]
        command: SecretsCommand,
    },
    /// Sync now: wake an account's (or every account's) sync and show progress until it is
    /// caught up. Exits 0 when caught up, 1 if a sync failed, 2 if an account needs a login.
    Sync {
        /// Account name or email address; every account when absent.
        account: Option<String>,
        /// Wake the sync and return at once.
        #[arg(long)]
        no_wait: bool,
    },
    /// Search the local store and Gmail's whole mailbox: one line per thread.
    Search {
        /// What to look for (Gmail's search syntax works for the Gmail part).
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        query: Vec<String>,
        /// Only this account (name or address).
        #[arg(long)]
        account: Option<String>,
        /// At most this many threads (up to 100).
        #[arg(long, default_value_t = rail_core::ops::DEFAULT_SEARCH_LIMIT)]
        limit: usize,
    },
    /// Check config, store, secrets, accounts, each account's sync and the daemon.
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
pub enum SecretsCommand {
    /// Keep tokens in the OS keyring, a private file or 1Password, moving every account's
    /// tokens there. Shows what changes and asks first, so it runs only on an interactive
    /// terminal.
    Set(SecretsSetArgs),
}

#[derive(Debug, clap::Args)]
pub struct SecretsSetArgs {
    /// Where tokens go from now on.
    pub backend: BackendArg,
    /// 1Password only: the vault Rail keeps its items in (needed unless config.toml has one).
    #[arg(long)]
    pub vault: Option<String>,
    /// 1Password only: the account, when several are signed in.
    #[arg(long)]
    pub account: Option<String>,
    /// Change the setting without moving tokens; each account then needs `rail account login`.
    #[arg(long)]
    pub no_migrate: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BackendArg {
    Keyring,
    File,
    #[value(name = "1password")]
    OnePassword,
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
    /// Store the client secret given in --client-secret in 1Password (the `[onepassword]`
    /// vault) and write an op:// reference into config.toml instead of the value.
    #[arg(long)]
    pub store_secret_in_1password: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum KindArg {
    Gmail,
    M365,
}
