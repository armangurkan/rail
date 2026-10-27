//! `[[saved_searches]]`: a name and the filter text it stands for.
//!
//! A query holds addresses and subject fragments, so it never reaches a log, an error or a
//! `Debug`: the hand-written `Debug` below prints the name and the query's length, as
//! `AccountConfig` does for `client_secret`.

use std::collections::HashSet;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::model::Config;
use crate::store::filter::MAX_FILTER_CHARS;

/// Saved searches Rail keeps. Beyond this the sidebar stops being a list.
pub const MAX_SAVED_SEARCHES: usize = 20;
/// Characters of a saved search's name.
pub const MAX_SAVED_SEARCH_NAME_CHARS: usize = 40;

/// One `[[saved_searches]]` table: `name` labels it, `query` is filter text as typed.
#[derive(Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
}

impl fmt::Debug for SavedSearch {
    /// The name and how long the query is: a query is mail content.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedSearch")
            .field("name", &self.name)
            .field("query_chars", &self.query.chars().count())
            .finish()
    }
}

/// Why a name is refused. It carries the rule, not the wording: the loader quotes the
/// offending value (as it does for an account name) while the prompt and the daemon show
/// §8's fixed sentence, and one `String` would force one of the two to say the other's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// Something other than an ASCII letter, digit, `-` or `_`.
    Charset,
    /// Over `MAX_SAVED_SEARCH_NAME_CHARS` characters.
    TooLong,
}

/// FR-5's rule: 1 to `MAX_SAVED_SEARCH_NAME_CHARS` ASCII letters, digits, `-` or `_`. The
/// charset leaves no quote, newline or control character in a name, so it is safe in TOML and
/// in a 26-column sidebar.
///
/// # Errors
/// Which rule the name breaks, so the caller can word it: the loader quotes the offending
/// value, the prompt and the daemon show `name_rule`'s fixed sentence.
pub fn check_name(name: &str) -> Result<(), NameProblem> {
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if name.chars().count() > MAX_SAVED_SEARCH_NAME_CHARS {
        return Err(NameProblem::TooLong);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(NameProblem::Charset);
    }
    Ok(())
}

/// The entry `name` would clash with, ignoring ASCII case, skipping the one `replaces` names
/// (also ignoring case) — FR-4's carve-out, which is what lets a rename keep its own name or
/// change only its case.
#[must_use]
pub fn duplicate_name<'a>(
    existing: &'a [SavedSearch],
    name: &str,
    replaces: Option<&str>,
) -> Option<&'a str> {
    existing
        .iter()
        .filter(|s| !replaces.is_some_and(|old| s.name.eq_ignore_ascii_case(old)))
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .map(|s| s.name.as_str())
}

/// Section 8's row for FR-5: the one sentence the prompt and the daemon both show when a name
/// is refused. Named once so the two cannot drift; the loader says something else on purpose,
/// quoting the offending value the way it does for an account name.
#[must_use]
pub fn name_rule() -> String {
    format!(
        "A saved search name must be 1 to {MAX_SAVED_SEARCH_NAME_CHARS} letters, digits, '-' or '_'."
    )
}

/// FR-4, FR-5 and FR-6 for one `saved_search.put`, as the prompt and the daemon both see it.
/// The text is §8's matching row verbatim, so the prompt's refusal and the daemon's error
/// cannot drift apart.
///
/// # Errors
/// The sentence to show: a malformed name, a name already saved, an over-long query, or the
/// cap reached (the last only when `replaces` is `None`, since a rename adds nothing).
pub fn check_put(
    existing: &[SavedSearch],
    name: &str,
    query: &str,
    replaces: Option<&str>,
) -> Result<(), String> {
    if check_name(name).is_err() {
        return Err(name_rule());
    }
    if duplicate_name(existing, name, replaces).is_some() {
        return Err(format!("A saved search named {name:?} already exists."));
    }
    if query.chars().count() > MAX_FILTER_CHARS {
        return Err(format!(
            "A saved search query must be at most {MAX_FILTER_CHARS} characters."
        ));
    }
    if replaces.is_none() && existing.len() >= MAX_SAVED_SEARCHES {
        return Err(format!(
            "Rail keeps at most {MAX_SAVED_SEARCHES} saved searches."
        ));
    }
    Ok(())
}

/// Every `[[saved_searches]]` table of a config file, in the loader's own wording: a bad or
/// duplicate name is quoted (as an account name is), an over-long query is reported by
/// position and name and **never by value** — a `ConfigError::Invalid` message lands in
/// `OpError.detail`, which `DoctorReport.config_error` carries and the MCP `rail_status` tool
/// returns to an agent.
pub(super) fn validate_saved_searches(config: &Config) -> Result<(), String> {
    let mut names = HashSet::new();
    for (index, search) in config.saved_searches.iter().enumerate() {
        if check_name(&search.name).is_err() {
            return Err(format!(
                "saved search name {:?} must be 1 to {MAX_SAVED_SEARCH_NAME_CHARS} letters, digits, '-' or '_'",
                search.name
            ));
        }
        if !names.insert(search.name.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate saved search name {:?} (names are compared ignoring case)",
                search.name
            ));
        }
        if search.query.chars().count() > MAX_FILTER_CHARS {
            return Err(format!(
                "saved_searches[{index}] ({:?}): the query is longer than {MAX_FILTER_CHARS} characters",
                search.name
            ));
        }
    }
    let found = config.saved_searches.len();
    if found > MAX_SAVED_SEARCHES {
        return Err(format!(
            "there are {found} [[saved_searches]] tables; Rail keeps at most {MAX_SAVED_SEARCHES} saved searches"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `count` saved searches, `s1`, `s2`, … each with a query of its own.
    fn many(count: usize) -> Vec<SavedSearch> {
        (1..=count)
            .map(|n| SavedSearch {
                name: format!("s{n}"),
                query: format!("subject:s{n}"),
            })
            .collect()
    }

    /// FR-6's query cap, in §8's wording, straight at `check_put` — the one function the
    /// prompt and the daemon both refuse through. Exactly `MAX_FILTER_CHARS` is accepted and
    /// one character more is not, so the boundary is pinned, not just the direction.
    #[test]
    fn a_query_one_character_over_the_cap_is_refused() {
        let at_cap = "x".repeat(MAX_FILTER_CHARS);
        assert_eq!(check_put(&[], "ok", &at_cap, None), Ok(()));
        assert_eq!(
            check_put(&[], "ok", &"x".repeat(MAX_FILTER_CHARS + 1), None),
            Err("A saved search query must be at most 1024 characters.".to_string())
        );
    }

    /// FR-6: the cap counts what the write would **add**, so at 20 a new name is refused in
    /// §8's wording while a rename, which adds nothing, goes through. The rename is still
    /// checked for everything else: FR-5's charset and FR-4's clash with another entry (its
    /// own entry is the one `replaces` skips).
    #[test]
    fn the_cap_stops_a_save_at_twenty_but_never_a_rename() {
        let full = many(MAX_SAVED_SEARCHES);
        assert_eq!(full.len(), 20);
        assert_eq!(
            check_put(&full, "one-more", "is:unread", None),
            Err("Rail keeps at most 20 saved searches.".to_string())
        );
        assert_eq!(
            check_put(&full, "renamed", "subject:s1", Some("s1")),
            Ok(())
        );
        assert_eq!(
            check_put(&full, "no spaces", "subject:s1", Some("s1")),
            Err(name_rule()),
            "a refusal at the cap is still the name rule, verbatim"
        );
        assert_eq!(
            check_put(&full, "S2", "subject:s1", Some("s1")),
            Err("A saved search named \"S2\" already exists.".to_string())
        );
    }
}
