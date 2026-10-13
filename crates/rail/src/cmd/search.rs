//! `rail search <query> [--account <name>] [--limit N]`: the store and each account's
//! mailbox on its server (Gmail or Microsoft 365), one line per thread (date, account, sender, subject), every mail field escaped.
//! Envelope fields only: no snippet or body is ever printed.

use std::process::ExitCode;

use rail_core::ops::op::Search;
use rail_core::ops::{SearchFailure, SearchHit, SearchRequest};
use rail_core::paths::Paths;

use crate::cmd::account::{escaped, outside, report};
use crate::cmd::text::{escape_field, utc_date};
use crate::daemon_link;

const SENDER_CHARS: usize = 30;
const SUBJECT_CHARS: usize = 120;

/// Prints the hits, newest first, on stdout (nothing else goes there), then on stderr each
/// account whose server could not be asked. Exits 1 when nothing was found and a server
/// search failed.
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
/// whether any did: every account syncs on its server, so each one skipped is a failure.
fn failure_report(failures: &[SearchFailure]) -> (Vec<String>, bool) {
    let lines = failures.iter().flat_map(failure_lines).collect();
    (lines, !failures.is_empty())
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

/// An account its provider could not be asked for: Rail's words, then the provider's (or the system's)
/// quoted on a line of their own, then the hint.
fn failure_lines(failure: &SearchFailure) -> Vec<String> {
    let error = &failure.error;
    let mut lines = vec![format!(
        "{}: not searched on {}: {}",
        escaped(&failure.account),
        failure.kind.provider_name(),
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
    use rail_core::config::AccountKind;
    use rail_core::error::ErrorClass;
    use rail_core::ops::{ErrorCode, HitSource, OpError, Untrusted};

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
            kind: AccountKind::Gmail,
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

    #[test]
    fn a_failed_microsoft_365_search_names_microsoft_365() {
        let failure = SearchFailure {
            account: "corp".into(),
            kind: AccountKind::M365,
            error: OpError::new(
                ErrorCode::Auth,
                ErrorClass::Permanent,
                "the account needs a new login",
            ),
        };
        assert_eq!(
            failure_lines(&failure)[0],
            "corp: not searched on Microsoft 365: the account needs a new login"
        );
    }

    fn skipped(account: &str, code: ErrorCode, message: &str) -> SearchFailure {
        SearchFailure {
            account: account.into(),
            kind: AccountKind::Gmail,
            error: OpError::new(code, ErrorClass::Permanent, message),
        }
    }

    /// Every skipped account is listed and counts as failed, an unsupported one too: no
    /// account is "searched locally only" now that Microsoft 365 is searched on its server.
    #[test]
    fn every_skipped_account_is_listed_and_counts_as_failed() {
        let corp = SearchFailure {
            kind: AccountKind::M365,
            ..skipped("corp", ErrorCode::Unsupported, "not in this version")
        };
        let work = skipped("work", ErrorCode::Provider, "Gmail refused the request");
        let (lines, failed) = failure_report(&[work, corp]);
        assert_eq!(
            lines,
            [
                "work: not searched on Gmail: Gmail refused the request",
                "corp: not searched on Microsoft 365: not in this version",
            ]
        );
        assert!(failed);
        assert_eq!(failure_report(&[]), (Vec::new(), false));
    }
}
