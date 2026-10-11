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
use crate::tui::run::{finish_run, run_app};
use crate::tui::screens::mail::{AccountEntry, MailContext, MailScreen};
use crate::tui::text::hyperlinks_supported;
use crate::tui::theme::Theme;
use crate::tui::theme::choice::{ColorDepth, ThemeChoice, ThemeError, color_depth};
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
    let choice =
        ThemeChoice::from_config(&config.general, text, &paths.config_file()).map_err(|e| {
            color_eyre::eyre::eyre!(
                "{e}
{}",
                hint_for(&e)
            )
        })?;
    let depth = color_depth(|name| std::env::var(name).ok());
    let theme = Theme::from_choice(choice, depth);
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        eprintln!("{MAIL_PIPE_HINT}");
        return Ok(ExitCode::from(2));
    }
    let accounts = AccountEntry::from_config(config);
    let screen = MailScreen::new(
        context(paths, config, keymap.clone(), depth, choice),
        accounts,
    )
    .with_panic_on_key(test_hooks::mail_panic());
    // The screen is dropped (it removes the files `o` wrote) before a signal may kill Rail.
    drop(run_app(screen, keymap, theme).await?);
    finish_run().await;
    Ok(ExitCode::SUCCESS)
}

fn context(
    paths: &Paths,
    config: &Config,
    keymap: Arc<Keymap>,
    depth: ColorDepth,
    theme_choice: ThemeChoice,
) -> MailContext {
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
        depth,
        theme_choice,
    }
}

/// The `hint:` line printed under a theme error.
fn hint_for(error: &ThemeError) -> &'static str {
    match error {
        ThemeError::UnknownTheme { .. } => {
            "hint: pick a theme with :theme inside Rail, or remove the key"
        }
        ThemeError::BadAccent { .. } => {
            "hint: accent takes a palette slot 0 to 15, a colour name such as \"blue\", or \"#rrggbb\"; :accent inside Rail picks one"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> ThemeError {
        let config = rail_core::config::parse(text, std::path::Path::new("/c")).unwrap();
        ThemeChoice::from_config(&config.general, text, std::path::Path::new("/c")).unwrap_err()
    }

    #[test]
    fn each_theme_error_has_its_own_hint() {
        assert!(hint_for(&error("[general]\ntheme = \"qqqqqqqqqq\"\n")).contains(":theme"));
        assert!(hint_for(&error("[general]\naccent = \"nope\"\n")).contains("slot 0 to 15"));
    }
}
