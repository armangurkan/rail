//! Every provider failure, classified. `message` is Rail's own text; `detail` is text from
//! outside Rail (Gmail's error message, a transport error stripped of its URL).

use std::time::Duration;

use crate::error::ErrorClass;
use crate::ops::{ErrorCode, OpError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// Network trouble, 5xx, 429, a rate-limit 403: retrying later can succeed.
    #[error("{message}")]
    Transient {
        message: String,
        detail: Option<String>,
        retry_after: Option<Duration>,
    },
    /// Any other refusal, or an answer Rail could not read.
    #[error("{message}")]
    Permanent {
        message: String,
        detail: Option<String>,
    },
    /// The message, or what was asked for, is gone (Gmail: HTTP 404).
    #[error("the mail provider no longer has it")]
    NotFound,
    /// The delta cursor is too old; the engine resyncs in full. An expected path.
    #[error("the sync cursor expired; a full resync is needed")]
    CursorExpired,
    /// The refresh token was refused or the API keeps refusing the access token.
    #[error("the account needs a new login")]
    NeedsAuth { detail: Option<String> },
}

impl ProviderError {
    pub fn transient(message: &str) -> Self {
        Self::Transient {
            message: message.to_string(),
            detail: None,
            retry_after: None,
        }
    }

    pub fn permanent(message: &str) -> Self {
        Self::Permanent {
            message: message.to_string(),
            detail: None,
        }
    }

    /// Attach outside text. A no-op for the variants that carry none.
    pub fn with_detail(mut self, text: String) -> Self {
        match &mut self {
            Self::Transient { detail, .. }
            | Self::Permanent { detail, .. }
            | Self::NeedsAuth { detail } => *detail = Some(text),
            Self::NotFound | Self::CursorExpired => {}
        }
        self
    }

    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Transient { .. } => ErrorClass::Transient,
            _ => ErrorClass::Permanent,
        }
    }

    /// The stable code the status and `accounts.last_error_code` record.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transient { .. } => "transient",
            Self::Permanent { .. } => "permanent",
            Self::NotFound => "not_found",
            Self::CursorExpired => "cursor_expired",
            Self::NeedsAuth { .. } => "needs_auth",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Transient { detail, .. }
            | Self::Permanent { detail, .. }
            | Self::NeedsAuth { detail } => detail.as_deref(),
            Self::NotFound | Self::CursorExpired => None,
        }
    }
}

impl From<ProviderError> for OpError {
    fn from(error: ProviderError) -> Self {
        let code = match &error {
            ProviderError::Transient { .. } => ErrorCode::Network,
            ProviderError::NotFound => ErrorCode::NotFound,
            ProviderError::NeedsAuth { .. } => ErrorCode::Auth,
            ProviderError::Permanent { .. } | ProviderError::CursorExpired => ErrorCode::Provider,
        };
        let mut op = OpError::new(code, error.class(), error.to_string());
        if let Some(detail) = error.detail() {
            op = op.with_detail(detail);
        }
        if matches!(error, ProviderError::NeedsAuth { .. }) {
            op = op.with_hint(
                "log in again with `rail account login <name>` or the rail_account_login tool",
            );
        }
        op
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_is_classified_and_coded() {
        let cases = [
            (
                ProviderError::transient("x"),
                ErrorClass::Transient,
                "transient",
            ),
            (
                ProviderError::permanent("x"),
                ErrorClass::Permanent,
                "permanent",
            ),
            (ProviderError::NotFound, ErrorClass::Permanent, "not_found"),
            (
                ProviderError::CursorExpired,
                ErrorClass::Permanent,
                "cursor_expired",
            ),
            (
                ProviderError::NeedsAuth { detail: None },
                ErrorClass::Permanent,
                "needs_auth",
            ),
        ];
        for (error, class, code) in cases {
            assert_eq!(error.class(), class, "{error:?}");
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn outside_text_only_ever_becomes_detail() {
        let error = ProviderError::permanent("Gmail refused the request (HTTP 400)")
            .with_detail("<script>evil</script>".to_string());
        let op = OpError::from(error);
        assert_eq!(op.message, "Gmail refused the request (HTTP 400)");
        assert_eq!(op.detail.unwrap().get(), "<script>evil</script>");
        let auth = OpError::from(ProviderError::NeedsAuth {
            detail: Some("Token has been expired or revoked.".into()),
        });
        assert_eq!(auth.code, ErrorCode::Auth);
        assert!(auth.hint.unwrap().contains("rail account login"));
    }
}
