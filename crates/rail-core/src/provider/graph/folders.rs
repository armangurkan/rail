//! The mailbox's folders: the root's children and, recursively, theirs (`childFolders`, for a
//! folder with `childFolderCount > 0`), hidden ones included, every page followed. Search
//! folders and the non-mail folders at the root are left out, with their children. The six
//! well-known folders are looked up by name and matched on id; every other folder is `User`.
//! Names are `Parent/Child`, as Gmail's nested labels are; a `/` in a folder's own name is
//! written `∕` (U+2215). A cap that cuts the listing is logged.

use std::collections::HashMap;

use super::wire::{FolderWire, IdOnly, Page};
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::{FolderKind, ProviderError, RemoteFolder, RemoteId};

/// The root folders Rail never syncs: not mail. Graph has no folder class, so these are
/// English names, and a mailbox in another language may still show them.
pub const SKIPPED_ROOTS: [&str; 4] = [
    "Conversation History",
    "RSS Feeds",
    "RSS Subscriptions",
    "Sync Issues",
];
/// The well-known names Rail looks up, in the delta's order, with the kinds they take.
pub const WELL_KNOWN: [(&str, FolderKind); 6] = [
    ("inbox", FolderKind::Inbox),
    ("sentitems", FolderKind::Sent),
    ("drafts", FolderKind::Drafts),
    ("archive", FolderKind::Archive),
    ("deleteditems", FolderKind::Trash),
    ("junkemail", FolderKind::Spam),
];
/// The most folders one listing keeps.
pub const MAX_FOLDERS: usize = 2_000;
/// The deepest nesting followed.
pub const MAX_FOLDER_DEPTH: usize = 16;
const SEARCH_FOLDER: &str = "#microsoft.graph.mailSearchFolder";
const MAX_FOLDER_PAGES: usize = 50;
const MAX_FOLDER_NAME_CHARS: usize = 255;
const FOLDER_PAGE: &str = "100";
/// What a `/` in a folder's own name is written as, so `Parent/Child` keeps one meaning.
pub const PATH_SLASH: char = '\u{2215}';

/// A folder whose children are still to be listed.
struct Pending {
    id: RemoteId,
    name: String,
    depth: usize,
}

/// A display name on one line, cut, its own `/` written `PATH_SLASH`.
fn clean(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' => PATH_SLASH,
            c if c.is_control() => ' ',
            c => c,
        })
        .take(MAX_FOLDER_NAME_CHARS)
        .collect()
}

fn skipped_root(wire: &FolderWire) -> bool {
    wire.display_name.as_deref().is_some_and(|name| {
        SKIPPED_ROOTS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name.trim()))
    })
}

/// Add `wire` under `parent` unless it is a search folder or its id is not one Rail sends,
/// and queue its children.
fn keep(
    wire: FolderWire,
    parent: Option<&Pending>,
    kinds: &HashMap<RemoteId, FolderKind>,
    found: &mut Vec<RemoteFolder>,
    pending: &mut Vec<Pending>,
) {
    if wire.odata_type.as_deref() == Some(SEARCH_FOLDER) || !graph_id(&wire.id) {
        return;
    }
    let own = clean(wire.display_name.as_deref().unwrap_or("(no name)"));
    let name = match parent {
        Some(parent) => format!("{}/{own}", parent.name),
        None => own,
    };
    let depth = parent.map_or(0, |p| p.depth + 1);
    if wire.child_folder_count.unwrap_or(0) > 0 {
        if depth < MAX_FOLDER_DEPTH {
            pending.push(Pending {
                id: wire.id.clone(),
                name: name.clone(),
                depth,
            });
        } else {
            tracing::warn!(
                folder = %wire.id,
                cap = MAX_FOLDER_DEPTH,
                "a Microsoft 365 folder's subfolders are nested deeper than Rail lists; left out"
            );
        }
    }
    found.push(RemoteFolder {
        kind: kinds.get(&wire.id).copied().unwrap_or(FolderKind::User),
        parent: parent.map(|p| p.id.clone()),
        remote_id: wire.id,
        name,
    });
}

impl GraphProvider {
    /// Every folder Rail syncs, sorted by name. A cap that cuts the listing is logged with the
    /// folder it stopped at; a folder whose children answer not found (deleted mid-listing)
    /// keeps its place and loses its subtree for this pass.
    pub(crate) async fn list_folders(&self) -> Result<Vec<RemoteFolder>, ProviderError> {
        let kinds = self.well_known().await?;
        let (mut found, mut pending) = (Vec::new(), Vec::new());
        for wire in self.folder_pages("me/mailFolders").await? {
            if !skipped_root(&wire) {
                keep(wire, None, &kinds, &mut found, &mut pending);
            }
        }
        while let Some(parent) = pending.pop() {
            if found.len() >= MAX_FOLDERS {
                tracing::warn!(
                    folder = %parent.id,
                    cap = MAX_FOLDERS,
                    "the Microsoft 365 folder listing reached its cap; the rest is left out"
                );
                break;
            }
            let path = format!("me/mailFolders/{}/childFolders", parent.id);
            let children = match self.folder_pages(&path).await {
                Ok(children) => children,
                Err(ProviderError::NotFound) => {
                    tracing::info!(
                        folder = %parent.id,
                        "a Microsoft 365 folder's subfolders answered not found; skipped this pass"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            for wire in children {
                keep(wire, Some(&parent), &kinds, &mut found, &mut pending);
            }
        }
        if found.len() > MAX_FOLDERS {
            tracing::warn!(
                cap = MAX_FOLDERS,
                "the Microsoft 365 folder listing reached its cap; the rest is left out"
            );
        }
        found.truncate(MAX_FOLDERS);
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    /// Every page of the folder listing at `path`, at most `MAX_FOLDER_PAGES`.
    async fn folder_pages(&self, path: &str) -> Result<Vec<FolderWire>, ProviderError> {
        let query = [
            ("includeHiddenFolders", "true".to_string()),
            ("$top", FOLDER_PAGE.to_string()),
        ];
        let mut page: Page<FolderWire> = self.api.get_json(path, &query, COST_REQUEST).await?;
        let mut all = std::mem::take(&mut page.value);
        for _ in 1..MAX_FOLDER_PAGES {
            let Some(next) = page.next_link.take() else {
                break;
            };
            page = self.api.get_json(&next, &[], COST_REQUEST).await?;
            all.append(&mut page.value);
        }
        if page.next_link.is_some() {
            tracing::warn!(
                path,
                cap = MAX_FOLDER_PAGES,
                "a Microsoft 365 folder listing has more pages than Rail reads; the rest is left out"
            );
        }
        Ok(all)
    }

    /// The ids of the well-known folders the mailbox has; a 404 means it has none of that name
    /// (`archive` is often missing).
    async fn well_known(&self) -> Result<HashMap<RemoteId, FolderKind>, ProviderError> {
        let mut kinds = HashMap::new();
        let query = [("$select", "id".to_string())];
        for (name, kind) in WELL_KNOWN {
            let path = format!("me/mailFolders/{name}");
            match self
                .api
                .get_json::<IdOnly>(&path, &query, COST_REQUEST)
                .await
            {
                Ok(found) if graph_id(&found.id) => {
                    kinds.insert(found.id, kind);
                }
                Ok(_) | Err(ProviderError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(kinds)
    }
}
