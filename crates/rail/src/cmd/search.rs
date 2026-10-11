//! `rail search <query> [--account <name>] [--limit N]`: the store and Gmail's whole
//! mailbox, one line per thread (date, account, sender, subject), every mail field escaped.
//! Envelope fields only: no snippet or body is ever printed.

use std::process::ExitCode;

use rail_core::ops::op::Search;
use rail_core::ops::{ErrorCode, SearchFailure, SearchHit, SearchRequest};
use rail_core::paths::Paths;

use crate::cmd::account::{escaped, outside, report};
use crate::cmd::text::{escape_field, utc_date};
use crate::daemon_link;

const SENDER_CHARS: usize = 30;
const SUBJECT_CHARS: usize = 120;
/// Said once, whatever the number of Microsoft 365 accounts.
const MICROSOFT_NOTE: &str =
    "Microsoft 365 accounts are searched locally only until Microsoft support arrives";

/// Prints the hits, newest first, on stdout (nothing else goes there), then on stderr each
/// account Gmail could not be asked for, with one note for all Microsoft 365 accounts. Exits
/// 1 when nothing was found and a Gmail search failed.
pub async fn run(
    paths: &Paths,
    query: String,
    account: Option<String>,
    limit: usize,
) -> color_eyre::Result<ExitCode> {
    let client = daemon_link::connector(paths)
        .client()
        .await
        .map_err(report)?;
    let request = SearchRequest {
        query,
        account,
        limit,
    };
    let found = client.call::<Search>(&request).await.map_err(report)?;
    if found.hits.is_empty() {
        eprintln!("No messages found.");
    }
    for hit in &found.hits {
        println!("{}", line(hit));
    }
    let (notes, failed) = failure_report(&found.failures);
    for note in notes {
        eprintln!("{note}");
    }
    Ok(if found.hits.is_empty() && failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// What the terminal says about the accounts the server search skipped or failed, and
/// whether any of them failed (rather than being Microsoft 365, searched locally only).
fn failure_report(failures: &[SearchFailure]) -> (Vec<String>, bool) {
    let (microsoft, failed): (Vec<&SearchFailure>, Vec<&SearchFailure>) = failures
        .iter()
        .partition(|f| f.error.code == ErrorCode::Unsupported);
    let mut lines: Vec<String> = failed.iter().flat_map(|f| failure_lines(f)).collect();
    if !microsoft.is_empty() {
        lines.push(MICROSOFT_NOTE.to_string());
    }
    (lines, !failed.is_empty())
}

/// One thread: `date  account  sender  subject`, the sender padded to a column.
pub fn line(hit: &SearchHit) -> String {
    format!(
        "{}  {}  {:<width$}  {}",
        utc_date(hit.date),
        escaped(&hit.account),
        escape_field(hit.sender.get(), SENDER_CHARS),
        escape_field(hit.subject.get(), SUBJECT_CHARS),
        width = SENDER_CHARS
    )
}

/// An account Gmail could not be asked for: Rail's words, then Gmail's (or the system's)
/// quoted on a line of their own, then the hint.
fn failure_lines(failure: &SearchFailure) -> Vec<String> {
    let error = &failure.error;
    let mut lines = vec![format!(
        "{}: not searched on Gmail: {}",
        escaped(&failure.account),
        escaped(&error.message)
    )];
    if let Some(detail) = &error.detail {
        lines.push(outside(detail.get()));
    }
    if let Some(hint) = &error.hint {
        lines.push(format!("hint: {}", escaped(hint)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::error::ErrorClass;
    use rail_core::ops::{HitSource, OpError, Untrusted};

    #[test]
    fn a_hit_is_one_escaped_line() {
        let hit = SearchHit {
            account: "work".into(),
            thread_id: "t1".into(),
            date: 1_700_000_000,
            sender: Untrusted::new("Dana \u{202E}evil <d@example.com>".into()),
            subject: Untrusted::new("Budget\r\nBcc: x \u{1b}]52;c;ZXZpbA==\u{7}".into()),
            source: HitSource::Server,
        };
        let text = line(&hit);
        assert!(
            !text.contains('\n') && !text.contains('\r') && !text.contains('\u{1b}'),
            "{text:?}"
        );
        assert!(!text.contains('\u{202E}'), "{text:?}");
        assert!(text.starts_with("2023-11-14  work"), "{text}");
        assert!(
            text.contains("Budget\\r\\nBcc: x \\u{1b}]52;c;ZXZpbA==\\u{7}"),
            "{text}"
        );
    }

    #[test]
    fn a_line_separator_in_a_subject_cannot_break_the_line() {
        let hit = SearchHit {
            account: "work".into(),
            thread_id: "t1".into(),
            date: 0,
            sender: Untrusted::new("a\u{2028}b".into()),
            subject: Untrusted::new("x".repeat(500) + "\u{2029}"),
            source: HitSource::Local,
        };
        let text = line(&hit);
        assert!(
            !text.contains('\u{2028}') && !text.contains('\u{2029}'),
            "{text:?}"
        );
        assert!(text.ends_with('…'), "a long subject is cut: {text}");
    }

    #[test]
    fn a_failed_gmail_search_is_rails_words_then_gmails_quoted() {
        let failure = SearchFailure {
            account: "work".into(),
            error: OpError::new(
                ErrorCode::Provider,
                ErrorClass::Transient,
                "could not reach Gmail",
            )
            .with_detail("dns\n\u{1b}[2Jerror"),
        };
        let lines = failure_lines(&failure);
        assert_eq!(
            lines[0],
            "work: not searched on Gmail: could not reach Gmail"
        );
        assert!(lines[1].starts_with("from outside Rail"), "{lines:?}");
        assert!(!lines.join("\n").contains('\u{1b}'), "{lines:?}");
    }

    fn skipped(account: &str, code: ErrorCode, message: &str) -> SearchFailure {
        SearchFailure {
            account: account.into(),
            error: OpError::new(code, ErrorClass::Permanent, message),
        }
    }

    /// T9-B: Microsoft 365 accounts give one note, not a failure each; other failures are
    /// still listed per account and still count as failed.
    #[test]
    fn microsoft_accounts_give_one_note_and_other_failures_stay_listed() {
        let corp = skipped(
            "corp",
            ErrorCode::Unsupported,
            "Microsoft 365 sync arrives later",
        );
        let lab = skipped(
            "lab",
            ErrorCode::Unsupported,
            "Microsoft 365 sync arrives later",
        );
        let (lines, failed) = failure_report(&[corp.clone(), lab.clone()]);
        assert_eq!(lines, [MICROSOFT_NOTE]);
        assert!(!failed, "searching locally only is not a failure");
        let work = skipped("work", ErrorCode::Provider, "Gmail refused the request");
        let (lines, failed) = failure_report(&[corp, work, lab]);
        assert_eq!(
            lines,
            [
                "work: not searched on Gmail: Gmail refused the request",
                MICROSOFT_NOTE
            ]
        );
        assert!(failed);
        assert_eq!(failure_report(&[]), (Vec::new(), false));
    }
}
