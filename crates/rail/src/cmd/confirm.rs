//! A change a person confirms at the terminal: `rail account remove` and `rail secrets set`.
//! Only a person at a terminal can confirm, so without one nothing changes. On a terminal,
//! the unconfirmed request makes the daemon say what would change (its `NotConfirmed` refusal
//! carries the text and the plan digest); that text is shown, the person is asked, and only on
//! a yes is the request sent again with `confirm: true` and the digest, so what runs is
//! exactly what the person read.

use std::io::{BufRead, IsTerminal, Write};

use color_eyre::eyre::bail;
use rail_core::daemon::DaemonClient;
use rail_core::ops::ErrorCode;
use rail_core::ops::op::Operation;

use super::account::report;

/// What one confirmed command says at each turn.
pub(crate) struct Words {
    /// Stdin or stdout is not a terminal: nothing was asked, nothing changed.
    pub not_a_terminal: &'static str,
    /// The question after the daemon's text, e.g. `Remove? [y/N] `.
    pub question: &'static str,
    /// Anything but a yes.
    pub declined: &'static str,
    /// The daemon refused without saying what would change.
    pub unexplained: &'static str,
    /// The daemon refused the yes: the plan changed while the question was on the screen.
    pub plan_changed: &'static str,
}

/// Refuse unless stdin and stdout are both a terminal.
pub(crate) fn require_terminal(words: &Words) -> color_eyre::Result<()> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Ok(());
    }
    bail!("{}", words.not_a_terminal)
}

/// Send `probe` (unconfirmed). When the daemon needs a confirmation, show its text, ask, and
/// on a yes send `confirmed(probe, plan)`. A probe the daemon answers at once (nothing needed
/// confirming) is the answer.
pub(crate) async fn confirmed_call<O: Operation>(
    client: &DaemonClient,
    probe: O::Request,
    confirmed: impl FnOnce(O::Request, String) -> O::Request,
    words: &Words,
) -> color_eyre::Result<O::Response> {
    let shown = match client.call::<O>(&probe).await {
        Err(error) if error.code == ErrorCode::NotConfirmed => match error.confirmation {
            Some(shown) => *shown,
            None => bail!("{}", words.unexplained),
        },
        answer => return answer.map_err(report),
    };
    let yes = ask(
        &shown.text,
        words.question,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )?;
    if !yes {
        bail!("{}", words.declined);
    }
    match client.call::<O>(&confirmed(probe, shown.plan)).await {
        Err(error) if error.code == ErrorCode::NotConfirmed => bail!("{}", words.plan_changed),
        answer => answer.map_err(report),
    }
}

/// Show `summary`, ask `question` and read one line; true only for a yes.
pub(crate) fn ask(
    summary: &str,
    question: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> std::io::Result<bool> {
    writeln!(output, "{summary}")?;
    write!(output, "{question}")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(confirms(&answer))
}

/// `y` or `yes`, in any case and with surrounding space, confirms; anything else, an empty
/// line and end of input included, does not.
pub(crate) fn confirms(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(typed: &str) -> (bool, String) {
        let mut screen = Vec::new();
        let yes = ask(
            "Move tokens?",
            "Change? [y/N] ",
            &mut typed.as_bytes(),
            &mut screen,
        )
        .unwrap();
        (yes, String::from_utf8(screen).unwrap())
    }

    #[test]
    fn the_text_and_question_are_shown_and_only_a_yes_confirms() {
        let (yes, screen) = asked("y\n");
        assert!(yes);
        assert_eq!(screen, "Move tokens?\nChange? [y/N] ");
        for no in ["\n", "", "n\n", "yess\n", "sure\n"] {
            assert!(!asked(no).0, "{no:?}");
        }
        assert!(asked(" YES \r\n").0);
    }

    #[test]
    fn only_y_or_yes_confirms() {
        for yes in ["y", "Y", "yes", "YES", " Yes \n", "y\r\n"] {
            assert!(confirms(yes), "{yes:?}");
        }
        for no in [
            "",
            "\n",
            "n",
            "no",
            "N",
            "yep",
            "yes please",
            "ok",
            "1",
            "true",
        ] {
            assert!(!confirms(no), "{no:?}");
        }
    }
}
