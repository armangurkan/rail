//! The store, read for the mail screen on blocking tasks through one read-only connection,
//! opened when first needed and opened again after any error.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rail_core::ops::{DraftId, OpError};
use rail_core::store::filter::Filter;
use rail_core::store::view::{AccountFilter, ListQuery, PageAt, Scope, SidebarCounts, ThreadPage};
use rail_core::store::{Store, StoreError};

/// How many address matches the recipient prompt shows at most.
const SUGGESTION_LIMIT: usize = 8;

use super::{MailEvent, job, refresh_job, send};
use crate::tui::app::Effect;

/// Why a read gave no answer.
#[derive(Debug)]
pub enum ReadError {
    /// No store yet, or one at an older version that the daemon has yet to migrate.
    NotSetUp,
    Failed(OpError),
    /// A newer list read was asked for before this one reached the store: it never ran.
    Stale,
}

/// Which threads the list shows, by account position in `config.toml`. Two folder choices
/// are the same folder when their account and `folder_id` match: `name` only labels the
/// header (a label renamed in Gmail keeps its id).
#[derive(Debug, Clone, Eq)]
pub enum ScopeChoice {
    Unified,
    Account(usize),
    /// A folder of one account; `name` is its (outside) name, for the header.
    Folder {
        account: usize,
        folder_id: i64,
        name: String,
    },
}

impl PartialEq for ScopeChoice {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unified, Self::Unified) => true,
            (Self::Account(a), Self::Account(b)) => a == b,
            (
                Self::Folder {
                    account: a,
                    folder_id: x,
                    ..
                },
                Self::Folder {
                    account: b,
                    folder_id: y,
                    ..
                },
            ) => a == b && x == y,
            _ => false,
        }
    }
}

/// One list read: the scope and filter, the configured accounts (addresses and names, in
/// order) to resolve them against, and where to read.
#[derive(Debug, Clone)]
pub struct ListRequest {
    pub seq: u64,
    pub scope: ScopeChoice,
    pub addresses: Vec<String>,
    pub names: Vec<String>,
    pub filter: Filter,
    pub at: PageAt,
    pub utc_offset: i32,
}

/// A page, where it was read, and each configured account's store id.
#[derive(Debug, Clone)]
pub struct ListOutcome {
    pub page: ThreadPage,
    pub at: PageAt,
    pub account_ids: Vec<Option<i64>>,
}

#[derive(Debug, Clone)]
pub struct SidebarOutcome {
    pub counts: SidebarCounts,
    pub account_ids: Vec<Option<i64>>,
}

/// Which message a reply or forward answers: this one, or a thread's latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyTarget {
    Message(i64),
    Thread(i64),
}

#[derive(Clone)]
pub struct StoreReader {
    db: PathBuf,
    conn: Arc<Mutex<Option<Store>>>,
    /// The newest list read's sequence number: an older one skips its query.
    latest_list: Arc<AtomicU64>,
    /// Reads that reached the store (tests).
    #[cfg(test)]
    queries: Arc<AtomicUsize>,
}

impl StoreReader {
    pub fn new(db: PathBuf) -> Self {
        Self {
            db,
            conn: Arc::default(),
            latest_list: Arc::default(),
            #[cfg(test)]
            queries: Arc::default(),
        }
    }

    /// Reads that reached the store so far (tests).
    #[cfg(test)]
    pub(crate) fn queries(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }

    /// Run `read` on the blocking pool with the reader connection.
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, ReadError> {
        self.read_unless_stale(None, read).await
    }

    /// `read`, unless `list_seq` is given and a newer list read was asked for by the time the
    /// connection is free: then it answers `Stale` without querying.
    async fn read_unless_stale<T: Send + 'static>(
        &self,
        list_seq: Option<u64>,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let (db, slot, latest) = (self.db.clone(), self.conn.clone(), self.latest_list.clone());
        #[cfg(test)]
        let queries = self.queries.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if list_seq.is_some_and(|seq| seq < latest.load(Ordering::SeqCst)) {
                return Err(ReadError::Stale);
            }
            #[cfg(test)]
            queries.fetch_add(1, Ordering::SeqCst);
            let store = match slot.take() {
                Some(store) => store,
                None => open(&db)?,
            };
            let result = read(&store).map_err(|e| ReadError::Failed(e.into()));
            if result.is_ok() {
                *slot = Some(store);
            }
            result
        })
        .await
        .unwrap_or_else(|_| {
            Err(ReadError::Failed(OpError::internal(
                "a store read failed to run",
            )))
        })
    }

    /// Record `seq` as the newest list read, so any older one still waiting skips itself.
    fn newest_list(&self, seq: u64) {
        self.latest_list.fetch_max(seq, Ordering::SeqCst);
    }

    /// The list read of `request`, or `Stale` when a newer one was asked for meanwhile.
    async fn read_list(&self, request: ListRequest) -> MailEvent {
        let seq = request.seq;
        let result = self
            .read_unless_stale(Some(seq), move |store| list(store, &request))
            .await;
        MailEvent::Listed { seq, result }
    }

    pub fn list(&self, request: ListRequest) -> Effect {
        self.newest_list(request.seq);
        let reader = self.clone();
        job(move |sink| async move {
            send(&sink, reader.read_list(request).await);
        })
    }

    pub fn sidebar(&self, seq: u64, addresses: Vec<String>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader.read(move |store| sidebar(store, &addresses)).await;
            send(&sink, MailEvent::Sidebar { seq, result });
        })
    }

    /// After a store change: the list read, the sidebar counts and, with `thread` (its
    /// sequence number and id), the open thread, in one job and in that order; then
    /// `RefreshEnded { seq: end }`, also when the job panics.
    pub fn refresh(
        &self,
        list: ListRequest,
        sidebar_seq: u64,
        thread: Option<(u64, i64)>,
        end: u64,
    ) -> Effect {
        self.newest_list(list.seq);
        let reader = self.clone();
        refresh_job(end, move |sink| async move {
            let addresses = list.addresses.clone();
            send(&sink, reader.read_list(list).await);
            let result = reader.read(move |store| sidebar(store, &addresses)).await;
            send(
                &sink,
                MailEvent::Sidebar {
                    seq: sidebar_seq,
                    result,
                },
            );
            if let Some((seq, thread_id)) = thread {
                send(&sink, reader.read_thread(seq, thread_id).await);
            }
        })
    }

    async fn read_thread(&self, seq: u64, thread_id: i64) -> MailEvent {
        let result = self.read(move |store| store.thread_detail(thread_id)).await;
        MailEvent::Thread {
            seq,
            thread_id,
            result,
        }
    }

    pub fn thread(&self, seq: u64, thread_id: i64) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            send(&sink, reader.read_thread(seq, thread_id).await);
        })
    }

    pub fn html(&self, message_id: i64) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.message_html(message_id))
                .await;
            send(&sink, MailEvent::Html { message_id, result });
        })
    }

    /// The pickers' labels for one account and what `threads` carry now.
    pub fn labels(&self, seq: u64, account_id: i64, threads: Vec<i64>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.label_choices(account_id, &threads))
                .await;
            send(&sink, MailEvent::Labels { seq, result });
        })
    }

    /// The reply source of `target`: a message, or the newest message of a thread.
    pub fn reply_source(&self, seq: u64, target: ReplyTarget) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| {
                    let message = match target {
                        ReplyTarget::Message(id) => Some(id),
                        ReplyTarget::Thread(thread) => store.latest_message_of(thread)?,
                    };
                    match message {
                        Some(id) => Ok(store.reply_source(id)?.map(Box::new)),
                        None => Ok(None),
                    }
                })
                .await;
            send(&sink, MailEvent::ReplySource { seq, result });
        })
    }

    /// The best address-book matches for `prefix`.
    pub fn suggest(&self, seq: u64, prefix: String) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.address_suggestions(&prefix, SUGGESTION_LIMIT))
                .await;
            send(&sink, MailEvent::Suggestions { seq, result });
        })
    }

    /// The Drafts folder of these accounts: local drafts and Gmail's, newest first.
    pub fn drafts_folder(&self, seq: u64, account_ids: Vec<i64>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.drafts_folder(&account_ids))
                .await;
            send(&sink, MailEvent::DraftsFolder { seq, result });
        })
    }

    /// One draft to resume. Its threading stays as stored: a save of it names no message.
    pub fn draft(&self, seq: u64, id: DraftId) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| Ok(store.draft(id)?.map(Box::new)))
                .await;
            send(&sink, MailEvent::Draft { seq, result });
        })
    }
}

fn open(db: &std::path::Path) -> Result<Store, ReadError> {
    if !db.exists() {
        return Err(ReadError::NotSetUp);
    }
    match Store::open_reader(db) {
        Ok(store) => Ok(store),
        // `open_reader` calls any other version "not set up"; a newer one is not.
        Err(StoreError::NotSetUp { found, expected }) if found > expected => {
            Err(ReadError::Failed(
                StoreError::TooNew {
                    found,
                    supported: expected,
                }
                .into(),
            ))
        }
        Err(StoreError::NotSetUp { .. }) => Err(ReadError::NotSetUp),
        Err(error) => Err(ReadError::Failed(error.into())),
    }
}

/// Each configured account's store id and the sidebar's counts.
fn sidebar(store: &Store, addresses: &[String]) -> Result<SidebarOutcome, StoreError> {
    let account_ids = store.account_ids(addresses)?;
    let known: Vec<i64> = account_ids.iter().flatten().copied().collect();
    let counts = store.sidebar_counts(&known)?;
    Ok(SidebarOutcome {
        counts,
        account_ids,
    })
}

/// The page `request` asks for, its scope and `account:` resolved to store ids.
fn list(store: &Store, request: &ListRequest) -> Result<ListOutcome, StoreError> {
    let account_ids = store.account_ids(&request.addresses)?;
    let id_of = |index: usize| account_ids.get(index).copied().flatten();
    let scope = match &request.scope {
        ScopeChoice::Unified => Some(Scope::Unified {
            accounts: account_ids.iter().flatten().copied().collect(),
        }),
        ScopeChoice::Account(index) => id_of(*index).map(|account_id| Scope::Inbox { account_id }),
        ScopeChoice::Folder {
            account, folder_id, ..
        } => id_of(*account).map(|account_id| Scope::Folder {
            account_id,
            folder_id: *folder_id,
        }),
    };
    let account = match &request.filter.account {
        None => AccountFilter::Any,
        Some(name) => request
            .names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .and_then(id_of)
            .map_or(AccountFilter::Nobody, AccountFilter::Only),
    };
    let page = match scope {
        Some(scope) => {
            let query = ListQuery {
                scope,
                filter: request.filter.clone(),
                account,
                utc_offset: request.utc_offset,
            };
            store.thread_page(&query, request.at)?
        }
        // The account has no row in the store yet: nothing synced, nothing to list.
        None => ThreadPage::default(),
    };
    Ok(ListOutcome {
        page,
        at: request.at,
        account_ids,
    })
}
