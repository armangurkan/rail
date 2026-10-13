//! Graph's JSON as Rail reads it. A field Graph may leave out is optional.

use serde::Deserialize;

/// One page of a collection, and where the next page is.
#[derive(Deserialize)]
pub(crate) struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub next_link: Option<String>,
}

/// A `mailFolder` (or a `mailSearchFolder`, told apart by its type).
#[derive(Deserialize)]
pub(crate) struct FolderWire {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "childFolderCount")]
    pub child_folder_count: Option<i64>,
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
}

/// An answer of which Rail reads only the id.
#[derive(Deserialize)]
pub(crate) struct IdOnly {
    pub id: String,
}
