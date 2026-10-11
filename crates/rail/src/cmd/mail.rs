//! `rail` with an account configured: the mail screen. It needs a terminal. The daemon is
//! reached from the screen's first effects, in the background, so the list reads from the
//! store at once and a daemon that cannot be reached only shows in the status bar.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use rail_core::config::Config;
use rail_core::ops::BrowserEnv;
use rail_core::paths::Paths;

use crate::tui::clock::Clock;
use crate::tui::data::{DaemonLink, StoreReader};
use crate::tui::effects::Opener;
use crate::tui::keys::Keymap;
use crate::tui::run::run_app;
use crate::tui::screens::mail::{AccountEntry, MailContext, MailScreen};
use crate::tui::text::hyperlinks_supported;
use crate::{browser, daemon_link, test_hooks};

/// Printed instead of the mail screen in a pipe.
pub const MAIL_PIPE_HINT: &str = "rail: the mail screen needs an interactive terminal; `rail search <query>` and `rail sync` work in scripts";

/// Open the mail screen over `config` (read from `text`, for `[keys]` line numbers).
pub async fn run(paths: &Paths, text: &str, config: &Config) -> color_eyre::Result<ExitCode> {
    let keymap = Arc::new(Keymap::from_config(
        &config.keys,
        text,
        &paths.config_file(),
    )?);
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        eprintln!("{MAIL_PIPE_HINT}");
        return Ok(ExitCode::from(2));
    }
    let accounts = AccountEntry::from_config(config);
    let screen = MailScreen::new(context(paths, config, keymap.clone()), accounts)
        .with_panic_on_key(test_hooks::mail_panic());
    run_app(screen, keymap).await?;
    Ok(ExitCode::SUCCESS)
}

fn context(paths: &Paths, config: &Config, keymap: Arc<Keymap>) -> MailContext {
    // The TUI opens a URL only when the user asks; it runs where the user is, so the browser
    // inherits this process's own display.
    let opener: Opener = Arc::new(|url| browser::launch(url, &BrowserEnv::default()));
    MailContext {
        paths: paths.clone(),
        keymap,
        store: StoreReader::new(paths.db_file()),
        link: DaemonLink::new(daemon_link::connector(paths)),
        opener,
        clock: Clock::system(),
        icons: config.general.icons,
        split_threshold: config.general.split_threshold,
        hyperlinks: hyperlinks_supported(|name| std::env::var(name).ok()),
    }
}
