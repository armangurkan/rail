//! The store, read for the mail screen on blocking tasks through one read-only connection,
//! opened when first needed and opened again after any error.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use rail_core::ops::OpError;
use rail_core::store::filter::Filter;
use rail_core::store::view::{AccountFilter, ListQuery, PageAt, Scope, SidebarCounts, ThreadPage};
use rail_core::store::{Store, StoreError};

use super::{MailEvent, job, send};
use crate::tui::app::Effect;

/// Why a read gave no answer.
#[derive(Debug)]
pub enum ReadError {
    /// No store yet, or one at an older version that the daemon has yet to migrate.
    NotSetUp,
    Failed(OpError),
}

/// Which threads the list shows, by account position in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Clone)]
pub struct StoreReader {
    db: PathBuf,
    conn: Arc<Mutex<Option<Store>>>,
}

impl StoreReader {
    pub fn new(db: PathBuf) -> Self {
        Self {
            db,
            conn: Arc::default(),
        }
    }

    /// Run `read` on the blocking pool with the reader connection.
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let (db, slot) = (self.db.clone(), self.conn.clone());
        tokio::task::spawn_blocking(move || {
            let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
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

    pub fn list(&self, request: ListRequest) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let seq = request.seq;
            let result = reader.read(move |store| list(store, &request)).await;
            send(&sink, MailEvent::Listed { seq, result });
        })
    }

    pub fn sidebar(&self, seq: u64, addresses: Vec<String>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader.read(move |store| sidebar(store, &addresses)).await;
            send(&sink, MailEvent::Sidebar { seq, result });
        })
    }

    /// After a store change: the list read and then the sidebar counts, in one job.
    pub fn refresh(&self, list: ListRequest, sidebar_seq: u64) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let (seq, addresses) = (list.seq, list.addresses.clone());
            let result = reader.read(move |store| self::list(store, &list)).await;
            send(&sink, MailEvent::Listed { seq, result });
            let result = reader.read(move |store| sidebar(store, &addresses)).await;
            send(
                &sink,
                MailEvent::Sidebar {
                    seq: sidebar_seq,
                    result,
                },
            );
        })
    }

    pub fn thread(&self, seq: u64, thread_id: i64) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.thread_detail(thread_id))
                .await;
            send(
                &sink,
                MailEvent::Thread {
                    seq,
                    thread_id,
                    result,
                },
            );
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
