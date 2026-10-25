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
        return Err(format!(
            "A saved search name must be 1 to {MAX_SAVED_SEARCH_NAME_CHARS} letters, digits, '-' or '_'."
        ));
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
