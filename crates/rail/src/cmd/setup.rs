//! The entry points that open the onboarding wizard: `rail` on first run, `rail setup`, and
//! `rail account add` without flags. The wizard never starts when stdout is not a terminal.

use std::io::IsTerminal;
use std::process::ExitCode;

use rail_core::config::{AccountKind, load_text_or_empty};
use rail_core::onboarding::Wizard;
use rail_core::paths::Paths;
use url::Url;

use crate::cmd::account::report as report_error;
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
    let (_, config) = load_text_or_empty(&paths.config_file())?;
    // The daemon starts (or fails, with its reason) before the terminal changes.
    let ctx = EffectContext::new(paths.clone());
    ctx.daemon.client().await.map_err(report_error)?;
    let screen = WizardScreen::new(Wizard::new(Some(config), kind_hint), ctx.clone());
    let screen = run_app(screen).await;
    // A quit mid-login leaves nothing waiting in the daemon.
    ctx.settle().await;
    let screen = screen?;
    Ok(report(
        screen.added(),
        screen.finished(),
        screen.shown_urls().0.as_ref(),
    ))
}

/// What the user sees once the terminal is back.
struct Outcome {
    code: u8,
    stdout: String,
    stderr: String,
}

/// A save is never half done at this point: the daemon never interrupts one. When the wizard
/// did not finish, the consent URL it last showed is printed, so a non-administrator can
/// forward it and a terminal that ignores the clipboard escape still has a copy path. The login
/// URL is never printed: it only works through the daemon's loopback listener for this login,
/// which the quit has cancelled. The consent URL carries no secret.
fn report(added: &[String], finished: bool, consent: Option<&Url>) -> ExitCode {
    let outcome = outcome(added, finished, consent);
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    ExitCode::from(outcome.code)
}

fn outcome(added: &[String], finished: bool, consent: Option<&Url>) -> Outcome {
    let mut out = Outcome {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    if finished {
        for name in added {
            out.stdout += &format!("Added account \"{name}\".\n");
        }
        out.stdout += "Run `rail doctor` to check everything.\n";
        return out;
    }
    if added.is_empty() {
        out.code = 1;
        out.stderr += "rail: setup cancelled; nothing was saved\n";
    } else {
        out.stderr += &format!("rail: setup interrupted; saved: {}\n", added.join(", "));
    }
    if let Some(url) = consent {
        out.stderr += &format!("rail: the last consent URL shown:\n{url}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use rail_core::error::ErrorClass;
    use rail_core::onboarding::{Event, Failure};

    use crate::tui::app::Msg;
    use crate::tui::screen::Screen;
    use crate::tui::test_util::{feed, gmail_own_app_keys, press, type_text, wizard_screen};

    fn cancelled_from_login(url: Option<&str>) -> WizardScreen {
        let mut screen = wizard_screen();
        for k in gmail_own_app_keys("me@gmail.com") {
            screen.update(Msg::Key(k));
        }
        if let Some(url) = url {
            feed(&mut screen, Event::LoginUrl(Url::parse(url).unwrap()));
        }
        press(&mut screen, KeyCode::Esc);
        assert!(!screen.finished());
        screen
    }

    #[test]
    fn a_cancel_from_the_login_step_does_not_print_the_login_url() {
        let url = "https://accounts.example/auth?state=abc&code_challenge=xyz";
        let screen = cancelled_from_login(Some(url));
        let out = outcome(
            screen.added(),
            screen.finished(),
            screen.shown_urls().0.as_ref(),
        );
        assert_eq!(out.code, 1);
        assert_eq!(out.stderr, "rail: setup cancelled; nothing was saved\n");
        assert!(!out.stderr.contains("accounts.example"), "{}", out.stderr);
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn a_consent_url_shown_to_a_non_administrator_is_printed_after_a_cancel() {
        let mut screen = wizard_screen();
        for code in [
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Enter,
        ] {
            press(&mut screen, code);
        }
        type_text(&mut screen, "me@contoso.com");
        for code in [KeyCode::Tab, KeyCode::Tab, KeyCode::Tab] {
            press(&mut screen, code);
        }
        type_text(&mut screen, "cid");
        press(&mut screen, KeyCode::Enter);
        let login = Url::parse("https://accounts.example/auth?state=abc").unwrap();
        feed(&mut screen, Event::LoginUrl(login));
        let failure = Failure {
            message: "AADSTS65001: consent required".into(),
            class: ErrorClass::Permanent,
            consent_required: true,
        };
        feed(&mut screen, Event::LoginFinished(Err(failure)));
        press(&mut screen, KeyCode::Esc);
        assert!(!screen.finished());

        let out = outcome(
            screen.added(),
            screen.finished(),
            screen.shown_urls().0.as_ref(),
        );

        assert!(
            out.stderr.contains("the last consent URL shown:\n"),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("/adminconsent"), "{}", out.stderr);
        assert!(!out.stderr.contains("accounts.example"), "{}", out.stderr);
    }

    #[test]
    fn nothing_extra_is_printed_when_no_url_was_shown() {
        let screen = cancelled_from_login(None);
        let out = outcome(
            screen.added(),
            screen.finished(),
            screen.shown_urls().0.as_ref(),
        );
        assert_eq!(out.stderr, "rail: setup cancelled; nothing was saved\n");
    }

    #[test]
    fn a_finished_setup_prints_the_accounts_and_no_urls() {
        let consent = Url::parse("https://login.microsoftonline.com/x/adminconsent").unwrap();
        let out = outcome(&["work".to_string()], true, Some(&consent));
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("Added account \"work\"."));
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn an_interrupted_setup_lists_what_was_saved_and_the_consent_url() {
        let consent = Url::parse("https://login.microsoftonline.com/x/adminconsent").unwrap();
        let out = outcome(&["work".to_string()], false, Some(&consent));
        assert_eq!(out.code, 0);
        assert!(out.stderr.contains("setup interrupted; saved: work"));
        assert!(out.stderr.contains("adminconsent"), "{}", out.stderr);
    }
}
