//! The combined cursor. Graph's delta is per folder and Rail keeps one `SyncCursor` per
//! account, so the cursor is a small JSON document of each folder's newest link and where its
//! walk stands:
//! `{"v":1,"after":<ms>,"folders":{"<id>":{"link":"<next or delta link>","state":"caught_up"}}}`.
//! A folder with no link has not started; `after` is the receive-time cutoff its initial delta
//! asks for. A round begins when every folder is caught up: all become due, and the calls of
//! the pass walk them within their page budget. A cursor over `MAX_CURSOR_BYTES`, one that
//! does not parse, or one of another version is expired: the engine resyncs in full.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::folders::WELL_KNOWN;
use super::graph_id;
use crate::provider::{FolderKind, ProviderError, RemoteFolder, RemoteId, SyncCursor};

/// The largest cursor Rail reads.
pub const MAX_CURSOR_BYTES: usize = 256 * 1024;
const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    /// The last page gave a delta link: nothing waits until the next round.
    CaughtUp,
    /// Mid-walk (the link is a next link), or not started (no link).
    Running,
    /// Caught up, and this round has not asked it yet.
    Due,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Folder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub state: State,
}

impl Folder {
    fn unstarted() -> Self {
        Self {
            link: None,
            state: State::Running,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphCursor {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    pub folders: BTreeMap<RemoteId, Folder>,
}

fn kind_rank(kind: FolderKind) -> usize {
    WELL_KNOWN
        .iter()
        .position(|(_, k)| *k == kind)
        .unwrap_or(WELL_KNOWN.len())
}

impl GraphCursor {
    /// Every folder of `folders`, none started; initial deltas ask for mail since `after`.
    pub(crate) fn start(folders: &[RemoteFolder], after: Option<i64>) -> Self {
        let folders = folders
            .iter()
            .map(|f| (f.remote_id.clone(), Folder::unstarted()))
            .collect();
        Self {
            v: VERSION,
            after,
            folders,
        }
    }

    pub(crate) fn decode(cursor: &SyncCursor) -> Result<Self, ProviderError> {
        if cursor.0.len() > MAX_CURSOR_BYTES {
            tracing::info!(
                bytes = cursor.0.len(),
                "the Microsoft 365 cursor is over its cap; resyncing"
            );
            return Err(ProviderError::CursorExpired);
        }
        let parsed: Self =
            serde_json::from_str(&cursor.0).map_err(|_| ProviderError::CursorExpired)?;
        if parsed.v != VERSION || !parsed.folders.keys().all(|id| graph_id(id)) {
            return Err(ProviderError::CursorExpired);
        }
        Ok(parsed)
    }

    pub(crate) fn encode(&self) -> Result<SyncCursor, ProviderError> {
        serde_json::to_string(self)
            .map(SyncCursor)
            .map_err(|_| ProviderError::permanent("Rail could not write the Microsoft 365 cursor"))
    }

    /// Follow the folder list: a new folder joins unstarted; a folder that is gone means a
    /// resync, whose sweep removes its mail.
    pub(crate) fn follow(&mut self, folders: &[RemoteFolder]) -> Result<(), ProviderError> {
        let gone = self
            .folders
            .keys()
            .any(|id| !folders.iter().any(|f| &f.remote_id == id));
        if gone {
            tracing::info!("a Microsoft 365 folder is gone; resyncing");
            return Err(ProviderError::CursorExpired);
        }
        for folder in folders {
            self.folders
                .entry(folder.remote_id.clone())
                .or_insert_with(Folder::unstarted);
        }
        Ok(())
    }

    /// A new round when none is under way: every caught-up folder becomes due.
    pub(crate) fn begin_round(&mut self) {
        if self.folders.values().all(|f| f.state == State::CaughtUp) {
            for folder in self.folders.values_mut() {
                folder.state = State::Due;
            }
        }
    }

    /// True while a folder is unstarted, mid-walk or due.
    pub(crate) fn more(&self) -> bool {
        self.folders.values().any(|f| f.state != State::CaughtUp)
    }

    /// Where folder `id` goes on after a page: a next link keeps it walking, a delta link
    /// ends its round. A page with neither is not one Rail can go on from.
    pub(crate) fn advance(
        &mut self,
        id: &str,
        next: Option<String>,
        delta: Option<String>,
    ) -> Result<(), ProviderError> {
        let folder = self
            .folders
            .get_mut(id)
            .ok_or_else(|| ProviderError::permanent("a Microsoft 365 folder left the cursor"))?;
        *folder = match (next, delta) {
            (Some(next), _) => Folder {
                link: Some(next),
                state: State::Running,
            },
            (None, Some(delta)) => Folder {
                link: Some(delta),
                state: State::CaughtUp,
            },
            (None, None) => {
                return Err(ProviderError::permanent(
                    "Microsoft 365's delta page had neither a next nor a delta link",
                ));
            }
        };
        Ok(())
    }

    /// The cursor's folders in walk order: the inbox, the other well-known folders, then the
    /// rest by name (`folders` gives kinds and names).
    pub(crate) fn order(&self, folders: &[RemoteFolder]) -> Vec<RemoteId> {
        let mut ids: Vec<RemoteId> = self.folders.keys().cloned().collect();
        ids.sort_by_cached_key(|id| match folders.iter().find(|f| &f.remote_id == id) {
            Some(f) => (kind_rank(f.kind), f.name.clone()),
            None => (usize::MAX, id.clone()),
        });
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str, kind: FolderKind) -> RemoteFolder {
        RemoteFolder {
            remote_id: id.into(),
            name: name.into(),
            kind,
            parent: None,
        }
    }

    #[test]
    fn a_cursor_round_trips_and_its_order_puts_the_inbox_first() {
        let folders = [
            folder("f-b", "Banking", FolderKind::User),
            folder("f-sent", "Sent Items", FolderKind::Sent),
            folder("f-a", "Accounts", FolderKind::User),
            folder("f-inbox", "Inbox", FolderKind::Inbox),
        ];
        let cursor = GraphCursor::start(&folders, Some(42));
        let decoded = GraphCursor::decode(&cursor.encode().unwrap()).unwrap();
        assert_eq!(decoded, cursor);
        assert_eq!(cursor.order(&folders), ["f-inbox", "f-sent", "f-a", "f-b"]);
        assert!(cursor.more(), "nothing started");
    }

    #[test]
    fn a_round_begins_only_when_every_folder_is_caught_up() {
        let folders = [
            folder("f-1", "One", FolderKind::User),
            folder("f-2", "Two", FolderKind::User),
        ];
        let mut cursor = GraphCursor::start(&folders, None);
        cursor.advance("f-1", None, Some("d1".into())).unwrap();
        cursor.begin_round();
        assert_eq!(
            cursor.folders["f-1"].state,
            State::CaughtUp,
            "f-2 still runs"
        );
        cursor.advance("f-2", None, Some("d2".into())).unwrap();
        assert!(!cursor.more());
        cursor.begin_round();
        assert!(cursor.folders.values().all(|f| f.state == State::Due));
        assert!(cursor.advance("f-1", None, None).is_err());
    }

    #[test]
    fn a_new_folder_joins_and_a_gone_one_expires_the_cursor() {
        let mut cursor = GraphCursor::start(&[folder("f-1", "One", FolderKind::User)], None);
        let now = [
            folder("f-1", "One", FolderKind::User),
            folder("f-2", "Two", FolderKind::User),
        ];
        cursor.follow(&now).unwrap();
        assert_eq!(cursor.folders["f-2"], Folder::unstarted());
        let fewer = [folder("f-2", "Two", FolderKind::User)];
        assert_eq!(cursor.follow(&fewer), Err(ProviderError::CursorExpired));
    }

    #[test]
    fn a_bad_folder_id_expires_the_cursor() {
        let bad = SyncCursor(r#"{"v":1,"folders":{"../x":{"state":"running"}}}"#.into());
        assert_eq!(GraphCursor::decode(&bad), Err(ProviderError::CursorExpired));
    }
}
