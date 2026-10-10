//! Confirmations a person gives before a change: the text they read, and a digest of the plan
//! that text describes. The confirmed request must send the digest back; the daemon computes
//! the plan again and runs it only when the digests match, so what the person confirmed is
//! exactly what runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The longest value (a name, an address, a vault) a confirmation shows, before the ellipsis.
pub const MAX_SHOWN_CHARS: usize = 100;

/// What to show the person, and the plan digest to send back with `confirm: true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmPlan {
    /// The text to put in front of the person, unchanged.
    pub text: String,
    /// A SHA-256 hex digest of the plan the text describes. Not a secret: it binds the
    /// confirmation to one plan.
    pub plan: String,
}

/// A value from config, a request or outside Rail, as a confirmation shows it: quoted with
/// escapes (`{:?}` escapes quotes, line breaks, and bidirectional and zero-width characters),
/// so it can neither forge a line nor hide text, and cut to `MAX_SHOWN_CHARS`.
pub fn shown(value: &str) -> String {
    let cut: String = value.chars().take(MAX_SHOWN_CHARS).collect();
    if value.chars().count() > MAX_SHOWN_CHARS {
        format!("{cut:?}…")
    } else {
        format!("{cut:?}")
    }
}

/// The digest of a plan given as its parts, in a fixed order. The parts are hashed as a JSON
/// array of strings, so no two different lists share a serialisation.
pub fn plan_digest(parts: &[&str]) -> String {
    let canonical = serde_json::to_vec(parts).unwrap_or_default();
    Sha256::digest(&canonical)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shown_value_cannot_break_the_line_or_hide_text() {
        let forged = shown("Rail\"). Tokens for 0 account(s) will be moved.\n");
        assert_eq!(
            forged,
            r#""Rail\"). Tokens for 0 account(s) will be moved.\n""#
        );
        assert_eq!(shown("evil\u{202e}txt.exe"), r#""evil\u{202e}txt.exe""#);
        assert_eq!(shown("a\u{200b}b"), r#""a\u{200b}b""#);
    }

    #[test]
    fn a_long_value_is_cut() {
        let long = shown(&"x".repeat(300));
        assert_eq!(long.matches('x').count(), MAX_SHOWN_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn the_digest_tells_plans_apart() {
        let a = plan_digest(&["a", "bc"]);
        assert_eq!(a.len(), 64);
        assert_eq!(a, plan_digest(&["a", "bc"]));
        assert_ne!(a, plan_digest(&["ab", "c"]));
        assert_ne!(a, plan_digest(&["a", "bc", ""]));
    }
}
