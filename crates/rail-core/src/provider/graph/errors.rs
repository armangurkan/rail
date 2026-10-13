//! Microsoft Graph's refusals, classified by HTTP status first and the envelope's
//! `error.code` second: Graph documents its statuses, not its mail codes. Graph's code and
//! message are outside text and only ever become `detail`.

use std::time::Duration;

use serde::Deserialize;

use crate::provider::ProviderError;

/// The longest code, and the longest message, a detail keeps.
const MAX_ERROR_TEXT: usize = 500;

/// A 403 that names access or consent.
pub const CONSENT_MESSAGE: &str = "Microsoft 365 refused access: an administrator may need to grant consent (see \"Work or school accounts and admin consent\" in the README)";
/// A mailbox Graph cannot serve.
pub const MAILBOX_MESSAGE: &str =
    "this mailbox cannot be reached through Microsoft Graph (on-premises, unlicensed or inactive)";

const MAILBOX_CODES: [&str; 2] = [
    "MailboxNotEnabledForRESTAPI",
    "MailboxNotHostedInExchangeOnline",
];
const CONSENT_CODES: [&str; 4] = [
    "ErrorAccessDenied",
    "AccessDenied",
    "Authorization_RequestDenied",
    "ConsentRequired",
];
const EXPIRED_CODES: [&str; 2] = ["syncStateNotFound", "resyncRequired"];

/// How an answer that was not a 2xx reads.
pub(crate) enum Refusal {
    /// A 401: the client refreshes the token once, then the account needs a login.
    Unauthorized(Option<String>),
    Failed(ProviderError),
}

#[derive(Deserialize, Default)]
struct Envelope {
    #[serde(default)]
    error: GraphError,
}

#[derive(Deserialize, Default)]
struct GraphError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

fn capped(text: String) -> String {
    text.chars().take(MAX_ERROR_TEXT).collect()
}

/// Graph's code (to match on) and what it said (code and message, for the detail).
fn read(body: &[u8]) -> (Option<String>, Option<String>) {
    let error = serde_json::from_slice::<Envelope>(body)
        .unwrap_or_default()
        .error;
    let code = error.code.map(capped);
    let detail = match (&code, error.message.map(capped)) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (Some(code), None) => Some(code.clone()),
        (None, message) => message,
    };
    (code, detail)
}

fn named(code: Option<&str>, names: &[&str]) -> bool {
    code.is_some_and(|code| names.iter().any(|name| code.eq_ignore_ascii_case(name)))
}

/// Rail's words for a refusal with this HTTP status.
pub(crate) fn refused_message(status: u16) -> String {
    format!("Microsoft 365 refused the request (HTTP {status})")
}

/// An answer with `status` (not a 2xx) and `body`, as Rail reads it.
pub(crate) fn refusal(status: u16, retry_after: Option<Duration>, body: &[u8]) -> Refusal {
    let (code, detail) = read(body);
    let code = code.as_deref();
    let error = match status {
        _ if named(code, &MAILBOX_CODES) => ProviderError::Permanent {
            message: MAILBOX_MESSAGE.to_string(),
            detail,
        },
        401 => return Refusal::Unauthorized(detail),
        403 if named(code, &CONSENT_CODES) => ProviderError::Permanent {
            message: CONSENT_MESSAGE.to_string(),
            detail,
        },
        404 => ProviderError::NotFound,
        410 => ProviderError::CursorExpired,
        400..=499 if named(code, &EXPIRED_CODES) => ProviderError::CursorExpired,
        429 | 500..=599 => ProviderError::Transient {
            message: format!("Microsoft 365 is busy or unavailable (HTTP {status})"),
            detail,
            retry_after,
        },
        _ => ProviderError::Permanent {
            message: refused_message(status),
            detail,
        },
    };
    Refusal::Failed(error)
}
