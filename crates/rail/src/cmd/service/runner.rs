//! Running service-manager commands (`systemctl`, `launchctl`): for real, or into a log for the
//! binary tests. Their output is outside text: capped, and shown only escaped and quoted.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;

use color_eyre::eyre::bail;

use crate::cmd::account::outside;
use crate::test_hooks;

use super::Cmd;

/// The most of a command's stdout or stderr Rail keeps.
pub const OUTPUT_CAP: usize = 4096;

/// How a command ended, with what it printed (each capped at `OUTPUT_CAP` bytes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn success() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }
}

/// Runs service-manager commands. `Err` when the program could not be run at all (not
/// installed, say), as opposed to running and failing.
pub trait Runner {
    fn run(&self, command: &Cmd) -> std::io::Result<Outcome>;
}

/// The real `systemctl` / `launchctl`, with stdin closed and their output captured.
struct System;

impl Runner for System {
    fn run(&self, command: &Cmd) -> std::io::Result<Outcome> {
        let Some((program, args)) = command.split_first() else {
            return Ok(Outcome::success());
        };
        let output = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok(Outcome {
            ok: output.status.success(),
            stdout: capped(&output.stdout),
            stderr: capped(&output.stderr),
        })
    }
}

/// `bytes` cut to `OUTPUT_CAP`, as text, without trailing whitespace.
pub fn capped(bytes: &[u8]) -> String {
    let kept = &bytes[..bytes.len().min(OUTPUT_CAP)];
    String::from_utf8_lossy(kept).trim_end().to_string()
}

/// Test hook: append each command to a file and report success; nothing runs.
struct Log(PathBuf);

impl Runner for Log {
    fn run(&self, command: &Cmd) -> std::io::Result<Outcome> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)?;
        writeln!(file, "{}", command.join(" "))?;
        Ok(Outcome::success())
    }
}

/// The real runner, or (debug builds, `RAIL_TEST_SERVICE_LOG`) the log.
pub fn runner() -> Box<dyn Runner> {
    match test_hooks::service_log() {
        Some(log) => Box::new(Log(log)),
        None => Box::new(System),
    }
}

/// Whether `runner` is the test log (which starts nothing, so there is no daemon to wait for).
pub fn logging_only() -> bool {
    test_hooks::service_log().is_some()
}

/// Rail's sentence for a command that did not succeed, with the tool's own words (or the
/// system's, when it could not run) on a line of their own as outside text.
pub fn failure(shown: &str, result: &std::io::Result<Outcome>) -> String {
    match result {
        Ok(outcome) if outcome.stderr.is_empty() => format!("`{shown}` failed"),
        Ok(outcome) => format!("`{shown}` failed\n  {}", outside(&outcome.stderr)),
        Err(error) => format!("could not run `{shown}`\n  {}", outside(&error.to_string())),
    }
}

fn succeeded(result: &std::io::Result<Outcome>) -> bool {
    matches!(result, Ok(outcome) if outcome.ok)
}

/// Run each command, printing it; the first failure stops and is returned.
pub fn run_strict(commands: &[Cmd], runner: &dyn Runner) -> color_eyre::Result<()> {
    for command in commands {
        let shown = command.join(" ");
        let result = runner.run(command);
        if !succeeded(&result) {
            bail!("{}", failure(&shown, &result));
        }
        println!("Ran: {shown}");
    }
    Ok(())
}

/// Run each command, printing it; a failure is printed, with the tool's words, and the rest
/// still run (for steps that may legitimately fail, such as stopping what is not loaded).
pub fn run_tolerant(commands: &[Cmd], runner: &dyn Runner) {
    for command in commands {
        let shown = command.join(" ");
        let result = runner.run(command);
        if succeeded(&result) {
            println!("Ran: {shown}");
        } else {
            println!("{} (carrying on)", failure(&shown, &result));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_capped_and_a_failure_shows_it_as_outside_text() {
        let long = vec![b'x'; OUTPUT_CAP * 2];
        assert_eq!(capped(&long).len(), OUTPUT_CAP);
        let failed = Ok(Outcome {
            ok: false,
            stdout: String::new(),
            stderr: "Failed to connect to bus\n\u{1b}[31m".into(),
        });
        let text = failure("systemctl --user x", &failed);
        assert!(
            text.starts_with("`systemctl --user x` failed\n  from outside Rail"),
            "{text}"
        );
        assert!(text.contains("\\n\\u{1b}[31m"), "{text}");
        assert_eq!(text.lines().count(), 2, "{text}");
        let missing = Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let text = failure("systemctl", &missing);
        assert!(
            text.starts_with("could not run `systemctl`\n  from outside Rail"),
            "{text}"
        );
    }
}
