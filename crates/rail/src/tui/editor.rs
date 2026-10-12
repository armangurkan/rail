//! The user's editor: which command, and running it on a file with the terminal handed over.

use std::path::Path;
use std::process::Command;

const FALLBACK: &str = "vi";

/// `general.editor`, else `$VISUAL`, else `$EDITOR`, else `vi`, split with shell-words rules
/// (quotes respected, no shell run). A blank or unparseable value is skipped.
pub fn command_line(configured: Option<&str>, var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let candidates = [configured.map(str::to_string), var("VISUAL"), var("EDITOR")];
    for candidate in candidates.into_iter().flatten() {
        if candidate.trim().is_empty() {
            continue;
        }
        if let Ok(words) = shell_words::split(&candidate)
            && !words.is_empty()
        {
            return words;
        }
    }
    vec![FALLBACK.to_string()]
}

/// Run `command` with `path` appended and wait for it; the exit code (`-1` for a signal).
/// The caller has already left the alternate screen and raw mode.
pub fn run(command: &[String], path: &Path) -> Result<i32, String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "no editor command".to_string())?;
    let status = Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .map_err(|e| format!("cannot run {program:?}: {e}"))?;
    Ok(status.code().unwrap_or(-1))
}
