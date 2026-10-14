//! Folders whose delta links keep running over `MAX_LINK_BYTES`: each start-over in a row is
//! counted, and after `MAX_LINK_RESETS` the folder is given up on for the rest of the daemon's
//! life, with a standing error that the sync status carries (the folder's name, outside text,
//! as its detail).

use std::collections::BTreeMap;

use super::cursor::Step;
use crate::provider::{ProviderError, RemoteFolder, RemoteId};

/// Start-overs in a row after which a folder is given up on.
pub const MAX_LINK_RESETS: u8 = 3;
/// The standing error's message; the folders' names go in its detail.
const GIVEN_UP: &str = "a folder cannot be synced: its sync link is longer than 4 KiB";

#[derive(Debug, Default)]
pub(super) struct Stuck {
    resets: BTreeMap<RemoteId, u8>,
    given_up: BTreeMap<RemoteId, String>,
}

impl Stuck {
    pub(super) fn given_up(&self, id: &str) -> bool {
        self.given_up.contains_key(id)
    }

    /// True while any folder is given up on.
    pub(super) fn any(&self) -> bool {
        !self.given_up.is_empty()
    }

    /// Count `step` for folder `id` (shown as `name`); true when the folder is given up on now.
    /// Only catching up clears the count: a page that walks on leaves it as it is, so a folder
    /// whose last page's link is always too long is given up on too.
    pub(super) fn count(&mut self, id: &str, step: Step, name: &str) -> bool {
        match step {
            Step::Walking => return false,
            Step::CaughtUp => {
                self.resets.remove(id);
                return false;
            }
            Step::StartedOver => {}
        }
        let resets = self.resets.entry(id.to_string()).or_insert(0);
        *resets = resets.saturating_add(1);
        if *resets < MAX_LINK_RESETS {
            return false;
        }
        tracing::warn!(
            folder = id,
            resets = *resets,
            "a Microsoft 365 folder's delta link stays over its cap; it is not synced until the daemon restarts"
        );
        self.given_up.insert(id.to_string(), name.to_string());
        true
    }

    /// The standing error: Rail's words, with the names of every folder given up on (outside
    /// text, in id order) as its detail.
    pub(super) fn error(&self) -> Option<ProviderError> {
        if self.given_up.is_empty() {
            return None;
        }
        let names: Vec<&str> = self.given_up.values().map(String::as_str).collect();
        Some(ProviderError::permanent(GIVEN_UP).with_detail(names.join(", ")))
    }
}

/// `id`'s name in `folders`, else the id.
pub(super) fn name_of(folders: &[RemoteFolder], id: &str) -> String {
    folders
        .iter()
        .find(|f| f.remote_id == id)
        .map_or_else(|| id.to_string(), |f| f.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_start_overs_in_a_row_count() {
        let mut stuck = Stuck::default();
        assert!(!stuck.count("f-1", Step::StartedOver, "One"));
        assert!(!stuck.count("f-1", Step::StartedOver, "One"));
        assert!(!stuck.count("f-1", Step::CaughtUp, "One"));
        assert!(!stuck.count("f-1", Step::StartedOver, "One"));
        assert!(!stuck.count("f-1", Step::StartedOver, "One"));
        assert!(
            stuck.error().is_none(),
            "a page that caught up in between starts the count again"
        );
        assert!(!stuck.count("f-1", Step::Walking, "One"));
        assert!(
            stuck.count("f-1", Step::StartedOver, "One"),
            "a page that walks on does not"
        );
        assert!(stuck.given_up("f-1"));
        let error = stuck.error().unwrap();
        assert_eq!(
            error.to_string(),
            "a folder cannot be synced: its sync link is longer than 4 KiB"
        );
        assert_eq!(error.detail(), Some("One"));
    }
}
