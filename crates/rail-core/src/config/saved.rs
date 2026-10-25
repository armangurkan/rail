//! `[[saved_searches]]`: a name and the filter text it stands for.
//!
//! A query holds addresses and subject fragments, so it never reaches a log, an error or a
//! `Debug`: the hand-written `Debug` below prints the name and the query's length, as
//! `AccountConfig` does for `client_secret`.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
