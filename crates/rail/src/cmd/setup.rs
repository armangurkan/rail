//! The entry points that open the onboarding wizard: `rail` on first run, `rail setup`, and
//! `rail account add` without flags. The wizard never starts when stdout is not a terminal.

use std::io::IsTerminal;
use std::process::ExitCode;

use rail_core::config::AccountKind;
use rail_core::onboarding::Wizard;
use rail_core::paths::Paths;

use crate::cmd::account::load_or_empty;
use crate::tui::effects::EffectContext;
use crate::tui::run::run_app;
use crate::tui::screens::wizard::WizardScreen;

/// Printed instead of starting the wizard in a pipe.
pub const PIPE_HINT: &str = "rail: the setup wizard needs an interactive terminal; use `rail account add <gmail|m365> --address <address> --client-id <id>` instead";

const NOT_BUILT: &str = "rail: the TUI is not built yet; run `rail --help` for available commands";

/// `rail` with no subcommand: the wizard on first run, otherwise the placeholder until the
/// Phase 4 TUI exists.
pub async fn first_run(paths: &Paths) -> color_eyre::Result<ExitCode> {
    if paths.config_file().exists() {
        eprintln!("{NOT_BUILT}");
        return Ok(ExitCode::from(2));
    }
    wizard(paths, None).await
}

/// Run the wizard full-screen. Everything it prints, it prints after the terminal is back.
pub async fn wizard(paths: &Paths, kind_hint: Option<AccountKind>) -> color_eyre::Result<ExitCode> {
    if !std::io::stdout().is_terminal() {
        eprintln!("{PIPE_HINT}");
        return Ok(ExitCode::from(2));
    }
    // A broken config.toml fails here, with its line number, before the terminal changes.
    let (_, config) = load_or_empty(paths)?;
    let screen = WizardScreen::new(
        Wizard::new(Some(config), kind_hint),
        EffectContext::new(paths.clone()),
    );
    let screen = run_app(screen).await?;
    Ok(report(screen.added(), screen.finished()))
}

/// What the user sees once the terminal is back. A save is never half done at this point:
/// the wizard holds Ctrl-C while saving.
fn report(added: &[String], finished: bool) -> ExitCode {
    if added.is_empty() {
        eprintln!("rail: setup cancelled; nothing was saved");
        return ExitCode::from(1);
    }
    if !finished {
        eprintln!("rail: setup interrupted; saved: {}", added.join(", "));
        return ExitCode::SUCCESS;
    }
    for name in added {
        println!("Added account \"{name}\".");
    }
    println!("Run `rail doctor` to check everything.");
    ExitCode::SUCCESS
}
