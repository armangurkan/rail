//! `rail account add|login|list|remove` as daemon clients. Output and exit codes are those of
//! the in-process commands they replace, with the columns of `list` sized to their entries.

use std::time::Duration;

use color_eyre::eyre::{bail, eyre};
use rail_core::config::{AccountKind, SecretsBackend};
use rail_core::daemon::DaemonClient;
use rail_core::ops::op::{
    AccountAddBegin, AccountLoginBegin, AccountRemove, AccountsList, PendingCancel, PendingWait,
};
use rail_core::ops::{
    AccountAddRequest, AccountRemoveRequest, AccountRemoveResponse, AccountSummary,
    AccountsListRequest, BeginResponse, CancelRequest, ClientSecretInput, LoginBeginRequest,
    OpError, PendingId, PendingOutcome, PendingState, SavedAccount, TokenState, WaitRequest,
    WaitResponse,
};
use rail_core::paths::Paths;
use tokio::signal::unix::{Signal, SignalKind, signal};

use super::confirm;
use crate::cli::{AddArgs, KindArg};
use crate::daemon_link;

impl From<KindArg> for AccountKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Gmail => AccountKind::Gmail,
            KindArg::M365 => AccountKind::M365,
        }
    }
}

/// An operation error as the terminal shows it: see `error_text`.
pub(crate) fn report(error: OpError) -> color_eyre::Report {
    eyre!(error_text(&error))
}

/// Rail's message, then the outside text (`detail`: a provider's, the system's or a parser's
/// words) escaped and quoted on a line of its own that says where it came from, then the hint.
pub(crate) fn error_text(error: &OpError) -> String {
    let mut lines = vec![error.message.clone()];
    if let Some(detail) = &error.detail {
        lines.push(outside(detail.get()));
    }
    if let Some(hint) = &error.hint {
        lines.push(format!("hint: {hint}"));
    }
    lines.join("\n")
}

/// Text Rail did not write, labelled as such and quoted.
pub(crate) fn outside(text: &str) -> String {
    format!(
        "from outside Rail (the provider or the system): {}",
        quoted(text)
    )
}

/// `text` in double quotes with control characters escaped, so outside text can neither break
/// the line nor send escape sequences to the terminal.
pub(crate) fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// The flag form of `rail account add`, or `None` when no flag asks for it and the wizard
/// should open (no arguments, or only a provider). A partial set of flags is an error that
/// names the missing one. `--client-secret op://...` is a 1Password reference.
pub(crate) fn new_account_from_args(
    args: AddArgs,
) -> color_eyre::Result<Option<AccountAddRequest>> {
    let Some(address) = args.address else {
        let others = [
            ("--client-id", args.client_id.is_some()),
            ("--client-secret", args.client_secret.is_some()),
            ("--tenant", args.tenant.is_some()),
            ("--name", args.name.is_some()),
            ("--color", args.color.is_some()),
            (
                "--store-secret-in-1password",
                args.store_secret_in_1password,
            ),
        ];
        if let Some((flag, _)) = others.iter().find(|(_, given)| *given) {
            bail!("{flag} needs --address");
        }
        return Ok(None);
    };
    let Some(kind) = args.kind else {
        bail!("--address needs a provider: `rail account add <gmail|m365> --address ...`");
    };
    let Some(client_id) = args.client_id else {
        bail!("--address needs --client-id");
    };
    Ok(Some(AccountAddRequest {
        kind: kind.into(),
        address,
        client_id: Some(client_id),
        client_secret: args.client_secret.map(ClientSecretInput::from_flag),
        tenant: args.tenant,
        name: args.name,
        color: args.color,
        store_secret_in_1password: args.store_secret_in_1password,
        open_browser: true,
    }))
}

async fn client(paths: &Paths) -> color_eyre::Result<DaemonClient> {
    daemon_link::connector(paths).client().await.map_err(report)
}

/// The daemon has already asked the browser to open; the URL is printed too, so a failed
/// opener still leaves the user a way in.
fn show(begun: &BeginResponse) {
    println!(
        "Open this URL in your browser if it does not open by itself:\n\n  {}\n",
        begun.url
    );
    if !begun.opened {
        eprintln!("rail: could not start the browser; open the URL above by hand");
    }
}

/// Ctrl-C from now on, delivered to `wait_or_cancel` instead of ending the process. Taken just
/// before the login begins, so a Ctrl-C that arrives while it starts still cancels it; one while
/// the daemon is still being reached ends the command as usual, with nothing begun.
fn interrupts() -> color_eyre::Result<Signal> {
    Ok(signal(SignalKind::interrupt())?)
}

/// Wait for the login to finish. Ctrl-C cancels it, unless the save has already started, in
/// which case the save is waited for: it is never interrupted.
async fn wait_or_cancel(
    client: &DaemonClient,
    id: &PendingId,
    interrupt: &mut Signal,
) -> color_eyre::Result<WaitResponse> {
    let wait = WaitRequest {
        id: id.clone(),
        timeout_secs: None,
    };
    loop {
        tokio::select! {
            answer = client.call::<PendingWait>(&wait) => {
                let answer = answer.map_err(report)?;
                if answer.state.is_finished() {
                    return Ok(answer);
                }
            }
            _ = interrupt.recv() => {
                let answer = cancel(client, id).await?;
                if answer.state != PendingState::Saving {
                    return Ok(answer);
                }
            }
        }
    }
}

/// The longest a Ctrl-C waits for the daemon to confirm the cancel.
pub(crate) const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

/// Send `pending.cancel`, bounded: a daemon that does not answer must not trap the user.
async fn cancel(client: &DaemonClient, id: &PendingId) -> color_eyre::Result<WaitResponse> {
    let request = CancelRequest { id: id.clone() };
    match tokio::time::timeout(CANCEL_TIMEOUT, client.call::<PendingCancel>(&request)).await {
        Ok(answer) => answer.map_err(report),
        Err(_) => Err(cancel_unanswered(CANCEL_TIMEOUT)),
    }
}

fn cancel_unanswered(waited: Duration) -> color_eyre::Report {
    eyre!(
        "the daemon did not answer the cancel within {} seconds; the login may still be pending\nhint: run `rail daemon status` to see it, or `rail daemon stop` to end it",
        waited.as_secs()
    )
}

/// The saved account, or the reason there is none.
fn saved(finished: WaitResponse) -> color_eyre::Result<SavedAccount> {
    match finished.state {
        PendingState::Done {
            outcome: PendingOutcome::Added { account } | PendingOutcome::LoggedIn { account },
        } => Ok(account),
        PendingState::Failed { error } => Err(eyre!("login failed: {}", error_text(&error))),
        PendingState::Cancelled => bail!("login cancelled; nothing was saved"),
        other => bail!("the login ended in an unexpected state: {other:?}"),
    }
}

fn warn_if_file_backend(paths: &Paths, account: &SavedAccount) {
    if account.token_backend == SecretsBackend::File.as_str() {
        eprintln!(
            "warning: tokens are stored in {} (mode 0600) because secrets = \"file\"; prefer the OS keyring where available",
            paths.secrets_file().display()
        );
    }
}

/// `rail account add` with flags: the daemon validates, logs in and saves.
pub async fn add(paths: &Paths, request: AccountAddRequest) -> color_eyre::Result<()> {
    let client = client(paths).await?;
    let mut interrupt = interrupts()?;
    let begun = client
        .call::<AccountAddBegin>(&request)
        .await
        .map_err(report)?;
    show(&begun);
    let account = saved(wait_or_cancel(&client, &begun.id, &mut interrupt).await?)?;
    warn_if_file_backend(paths, &account);
    println!(
        "Added {} ({}) as \"{}\". Tokens stored in the {}.",
        account.address,
        account.kind.as_str(),
        account.name,
        account.token_backend
    );
    Ok(())
}

/// `rail account login <name-or-address>`: log in again and replace the stored tokens.
/// An unknown account fails before the browser opens.
pub async fn relogin(paths: &Paths, who: &str) -> color_eyre::Result<()> {
    let client = client(paths).await?;
    let mut interrupt = interrupts()?;
    let request = LoginBeginRequest {
        account: who.to_string(),
        open_browser: true,
    };
    let begun = client
        .call::<AccountLoginBegin>(&request)
        .await
        .map_err(report)?;
    show(&begun);
    let account = saved(wait_or_cancel(&client, &begun.id, &mut interrupt).await?)?;
    warn_if_file_backend(paths, &account);
    println!(
        "Logged in to {} ({}) as \"{}\". Tokens stored in the {}.",
        account.address,
        account.kind.as_str(),
        account.name,
        account.token_backend
    );
    Ok(())
}

/// `rail account list`. Token presence comes from the daemon, which asks the store whether
/// tokens exist without reading them.
pub async fn list(paths: &Paths) -> color_eyre::Result<()> {
    let listed = client(paths)
        .await?
        .call::<AccountsList>(&AccountsListRequest {})
        .await
        .map_err(report)?;
    if listed.accounts.is_empty() {
        println!("No accounts configured. Run `rail account add --help`.");
        return Ok(());
    }
    print!("{}", table(&listed.accounts));
    Ok(())
}

/// `rail account remove <name-or-address>`: removes only after a person at a terminal read
/// the daemon's text and said yes (see `confirm`).
pub async fn remove(paths: &Paths, who: &str) -> color_eyre::Result<()> {
    confirm::require_terminal(&REMOVE_WORDS)?;
    let client = client(paths).await?;
    let probe = AccountRemoveRequest {
        account: who.to_string(),
        confirm: false,
        plan: None,
    };
    let confirmed = |probe: AccountRemoveRequest, plan| AccountRemoveRequest {
        confirm: true,
        plan: Some(plan),
        ..probe
    };
    let removed =
        confirm::confirmed_call::<AccountRemove>(&client, probe, confirmed, &REMOVE_WORDS).await?;
    print_removed(&removed);
    Ok(())
}

const REMOVE_WORDS: confirm::Words = confirm::Words {
    not_a_terminal: "refusing to remove an account without asking: stdin and stdout are not both a terminal; nothing was removed\nhint: run `rail account remove` yourself in a terminal",
    question: "Remove? [y/N] ",
    declined: "nothing was removed",
    unexplained: "the daemon did not say what would be removed; nothing was removed",
    // The daemon refused the yes, which was for the account as it was.
    plan_changed: "the account changed while you answered; nothing was removed — run `rail account remove` again",
};

fn print_removed(removed: &AccountRemoveResponse) {
    let entry = format!(
        "Removed \"{}\" <{}> from config.toml",
        removed.name, removed.address
    );
    match &removed.token_error {
        None => println!("{entry} and deleted its tokens. Its mail stays in Rail's store."),
        Some(error) => {
            println!("{entry}. Its mail stays in Rail's store.");
            eprintln!(
                "warning: its tokens could not be deleted\n{}",
                outside(error.get())
            );
        }
    }
}

/// The account table, its name and address columns as wide as their longest entry.
pub(crate) fn table(accounts: &[AccountSummary]) -> String {
    let width = |header: &str, cell: fn(&AccountSummary) -> &str| {
        accounts
            .iter()
            .map(|a| cell(a).chars().count())
            .chain([header.len()])
            .max()
            .unwrap_or(0)
    };
    let name = width("NAME", |a| a.name.as_str());
    let address = width("ADDRESS", |a| a.address.as_str());
    let mut out = format!(
        "{:<name$} {:<6} {:<address$} TOKENS\n",
        "NAME", "KIND", "ADDRESS"
    );
    for account in accounts {
        let tokens = match &account.tokens {
            TokenState::Present => "yes".to_string(),
            TokenState::Missing => "missing".to_string(),
            TokenState::Error { message } => format!("error {}", outside(message.get())),
        };
        out += &format!(
            "{:<name$} {:<6} {:<address$} {tokens}\n",
            account.name,
            account.kind.as_str(),
            account.address
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::config::AiSend;
    use rail_core::ops::Secret;

    fn parse_add(args: &[&str]) -> AddArgs {
        use crate::cli::{AccountCommand, Cli, Command};
        use clap::Parser;
        match Cli::try_parse_from(args).unwrap().command {
            Some(Command::Account {
                command: AccountCommand::Add(add),
            }) => add,
            other => panic!("{other:?}"),
        }
    }

    fn from_args(args: &[&str]) -> color_eyre::Result<Option<AccountAddRequest>> {
        let mut all = vec!["rail", "account", "add"];
        all.extend_from_slice(args);
        new_account_from_args(parse_add(&all))
    }

    fn message(args: &[&str]) -> String {
        from_args(args)
            .expect_err("partial set refused")
            .to_string()
    }

    #[test]
    fn no_provider_and_no_flags_opens_the_wizard() {
        assert!(from_args(&[]).unwrap().is_none());
    }

    #[test]
    fn a_provider_alone_opens_the_wizard() {
        assert!(from_args(&["gmail"]).unwrap().is_none());
    }

    #[test]
    fn address_and_client_id_with_a_provider_build_the_account() {
        let flags = [
            "m365",
            "--address",
            "a@b.c",
            "--client-id",
            "x",
            "--tenant",
            "t",
        ];
        let new = from_args(&flags).unwrap().unwrap();
        assert_eq!(new.kind, AccountKind::M365);
        assert_eq!(new.address, "a@b.c");
        assert_eq!(new.client_id.as_deref(), Some("x"));
        assert_eq!(new.tenant.as_deref(), Some("t"));
        assert_eq!(new.name, None);
        assert!(new.open_browser);
    }

    #[test]
    fn a_client_secret_flag_may_be_a_1password_reference() {
        let base = [
            "gmail",
            "--address",
            "a@b.c",
            "--client-id",
            "x",
            "--client-secret",
        ];
        let mut reference = base.to_vec();
        reference.push("op://Rail/google/client_secret");
        assert_eq!(
            from_args(&reference).unwrap().unwrap().client_secret,
            Some(ClientSecretInput::Reference(
                "op://Rail/google/client_secret".into()
            ))
        );
        let mut value = base.to_vec();
        value.push("GOCSPX-x");
        assert_eq!(
            from_args(&value).unwrap().unwrap().client_secret,
            Some(ClientSecretInput::Value(Secret::new("GOCSPX-x")))
        );
    }

    /// Final review F4: the flag reaches the request; without it the request stays false. On
    /// its own (no --address) it names the missing flag like every other.
    #[test]
    fn store_secret_in_1password_maps_to_the_request() {
        let base = [
            "gmail",
            "--address",
            "a@b.c",
            "--client-id",
            "x",
            "--client-secret",
            "GOCSPX-x",
        ];
        assert!(!from_args(&base).unwrap().unwrap().store_secret_in_1password);
        let mut flagged = base.to_vec();
        flagged.push("--store-secret-in-1password");
        assert!(
            from_args(&flagged)
                .unwrap()
                .unwrap()
                .store_secret_in_1password
        );
        let msg = message(&["gmail", "--store-secret-in-1password"]);
        assert!(
            msg.contains("--store-secret-in-1password needs --address"),
            "{msg}"
        );
    }

    #[test]
    fn address_without_a_provider_names_the_provider() {
        let msg = message(&["--address", "a@b.c", "--client-id", "x"]);
        assert!(msg.contains("provider"), "{msg}");
    }

    #[test]
    fn address_without_a_client_id_names_the_flag() {
        let msg = message(&["gmail", "--address", "a@b.c"]);
        assert!(msg.contains("--client-id"), "{msg}");
    }

    #[test]
    fn any_other_flag_without_an_address_names_the_address_flag() {
        for flag in [
            ["--client-id", "x"],
            ["--client-secret", "x"],
            ["--tenant", "x"],
            ["--name", "x"],
            ["--color", "x"],
        ] {
            let msg = message(&["gmail", flag[0], flag[1]]);
            assert!(msg.contains("--address"), "{flag:?}: {msg}");
        }
    }

    #[test]
    fn a_report_carries_the_hint() {
        let error = OpError::invalid_input("bad").with_hint("do this");
        assert_eq!(report(error).to_string(), "bad\nhint: do this");
    }
    #[test]
    fn outside_detail_is_quoted_escaped_and_on_its_own_line() {
        let error = OpError::invalid_input("the provider refused")
            .with_detail("bad\nline \u{1b}[2J \"quoted\"")
            .with_hint("try again");
        let text = error_text(&error);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[0], "the provider refused");
        assert_eq!(
            lines[1],
            r#"from outside Rail (the provider or the system): "bad\nline \u{1b}[2J \"quoted\"""#
        );
        assert_eq!(lines[2], "hint: try again");
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    fn summary(name: &str, address: &str, tokens: TokenState) -> AccountSummary {
        AccountSummary {
            name: name.into(),
            kind: AccountKind::Gmail,
            address: address.into(),
            color: None,
            tokens,
            ai_send: AiSend::Confirm,
        }
    }

    #[test]
    fn long_names_and_addresses_keep_the_columns_aligned() {
        let text = table(&[
            summary(
                "a-very-long-account-name",
                "someone.with.a.long.address@example.com",
                TokenState::Present,
            ),
            summary("w", "w@x.io", TokenState::Present),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        let column = |line: &str, word: &str| line.find(word).unwrap();
        assert_eq!(column(lines[0], "KIND"), column(lines[1], "gmail"));
        assert_eq!(column(lines[1], "gmail"), column(lines[2], "gmail"));
        assert_eq!(column(lines[0], "TOKENS"), column(lines[2], "yes"));
        assert!(lines[2].starts_with("w "), "{text}");
    }

    #[test]
    fn a_token_store_error_in_the_table_is_quoted() {
        let message = "locked\n\u{1b}[31m".to_string().into();
        let text = table(&[summary("w", "w@x.io", TokenState::Error { message })]);
        assert_eq!(text.lines().count(), 2, "{text:?}");
        assert!(
            text.contains(
                r#"error from outside Rail (the provider or the system): "locked\n\u{1b}[31m""#
            ),
            "{text:?}"
        );
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    #[test]
    fn an_unanswered_cancel_says_the_login_may_still_be_pending() {
        let text = cancel_unanswered(CANCEL_TIMEOUT).to_string();
        assert!(text.starts_with("the daemon did not answer the cancel within 5 seconds"));
        assert!(text.contains("the login may still be pending"), "{text}");
        assert!(text.contains("\nhint: run `rail daemon status`"), "{text}");
    }
}
