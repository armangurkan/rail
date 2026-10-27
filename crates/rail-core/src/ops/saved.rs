//! `saved_search.put` and `saved_search.remove`: the `[[saved_searches]]` tables of
//! `config.toml`, written by the daemon.
//!
//! A saved query is mail content, so no request or response type here derives `Debug`. The one
//! response that needs one (a test compares it) writes each entry through `SavedSearch`'s own
//! `Debug`, which prints the name and the query's length.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::SavedSearch;

/// Save a search, or rename the one `replaces` names (keeping its position).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SavedSearchPutRequest {
    pub name: String,
    /// The filter text to save. A rename leaves it out (`None`) so the query in the file is
    /// kept as written, however the client's copy of it has aged; a new search must carry one.
    #[serde(default)]
    pub query: Option<String>,
    /// The existing name this entry replaces; `None` saves a new search.
    pub replaces: Option<String>,
}

/// Remove the search named `name`. The mail it finds is not touched.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SavedSearchRemoveRequest {
    pub name: String,
}

/// Every saved search after the write, in the file's order. Both operations answer it.
#[derive(Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SavedSearchListResponse {
    pub searches: Vec<SavedSearch>,
}

impl fmt::Debug for SavedSearchListResponse {
    /// Names and query lengths only, through `SavedSearch`'s own `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedSearchListResponse")
            .field("searches", &self.searches)
            .finish()
    }
}
