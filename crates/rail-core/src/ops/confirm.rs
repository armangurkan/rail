//! Confirmations a person gives before a change: the text they read, and a digest of the plan
//! that text describes. The confirmed request must send the digest back; the daemon computes
//! the plan again and runs it only when the digests match, so what the person confirmed is
//! exactly what runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The longest vault or 1Password account name Rail takes. Longer ones are refused at input,
/// so a confirmation never has to cut a value.
pub const MAX_NAME_CHARS: usize = 100;

/// What to show the person, and the plan digest to send back with `confirm: true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmPlan {
    /// The text to put in front of the person, unchanged.
    pub text: String,
    /// A SHA-256 hex digest of the plan the text describes. Not a secret: it binds the
    /// confirmation to one plan.
    pub plan: String,
}

/// A value from config, a request or outside Rail, as a confirmation shows it: in double
/// quotes, in plain ASCII (`str::escape_default`: quotes, backslashes and control characters
/// escaped, every non-ASCII character as `\u{..}`), so it can neither forge a line, hide text
/// nor pass a homoglyph off as a Latin letter. Never cut: two values that differ always look
/// different; lengths are limited where the values come in.
pub fn shown(value: &str) -> String {
    format!("\"{}\"", value.escape_default())
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

    /// Plain ASCII, so a homoglyph cannot pass for a Latin letter.
    #[test]
    fn a_homoglyph_is_shown_as_its_code_point() {
        assert_eq!(shown("R\u{430}il"), r#""R\u{430}il""#);
        assert!(shown("caf\u{e9} \u{1f600}").is_ascii());
    }

    /// Never cut, so two long values cannot look the same.
    #[test]
    fn two_long_values_sharing_a_prefix_look_different() {
        let common = "v".repeat(MAX_NAME_CHARS);
        let (a, b) = (format!("{common}a"), format!("{common}b"));
        assert_ne!(shown(&a), shown(&b));
        assert!(shown(&a).contains(&a));
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
