//! The store, read for the mail screen on blocking tasks through one read-only connection,
//! opened when first needed and opened again after any error.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rail_core::ops::{DraftId, OpError};
use rail_core::provider::FolderKind;
use rail_core::store::filter::Filter;
use rail_core::store::view::{
    AccountFilter, ListQuery, PageAt, Scope, SidebarCounts, ThreadKey, ThreadPage,
};
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
    /// The starred threads of every account.
    Starred,
    /// The threads a snooze holds, in every account.
    Snoozed,
    /// Every account's Sent.
    Sent,
    /// Every account's Trash.
    Trash,
    /// Every account's drafts, in the Drafts pane.
    Drafts,
    Account(usize),
    /// A folder of one account; `name` is its (outside) name, for the header, and `kind` its
    /// kind, for the header's unread count. Neither names the folder.
    Folder {
        account: usize,
        folder_id: i64,
        name: String,
        kind: FolderKind,
    },
    /// A saved search: every folder of every account narrowed by its filter. `index` is its
    /// position in `config.toml`, which is its identity; `name` labels the header.
    Saved {
        index: usize,
        name: String,
    },
}

impl PartialEq for ScopeChoice {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unified, Self::Unified)
            | (Self::Starred, Self::Starred)
            | (Self::Snoozed, Self::Snoozed)
            | (Self::Sent, Self::Sent)
            | (Self::Trash, Self::Trash)
            | (Self::Drafts, Self::Drafts) => true,
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
            (Self::Saved { index: a, .. }, Self::Saved { index: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl ScopeChoice {
    /// A merged mailbox's name: `All inboxes`, `Starred`, `Snoozed`, `Drafts`, `Sent`, `Trash`.
    pub fn unified_name(&self) -> Option<&'static str> {
        match self {
            Self::Unified => Some("All inboxes"),
            Self::Starred => Some("Starred"),
            Self::Snoozed => Some("Snoozed"),
            Self::Drafts => Some("Drafts"),
            Self::Sent => Some("Sent"),
            Self::Trash => Some("Trash"),
            Self::Account(_) | Self::Folder { .. } | Self::Saved { .. } => None,
        }
    }

    /// Every account's mail together.
    pub fn is_unified(&self) -> bool {
        self.unified_name().is_some()
    }

    /// The header counts unread threads here: not in Sent, Trash, Drafts or Snoozed, merged or
    /// an account's own.
    pub fn counts_unread(&self) -> bool {
        match self {
            Self::Sent | Self::Trash | Self::Drafts | Self::Snoozed => false,
            Self::Folder { kind, .. } => !matches!(
                kind,
                FolderKind::Sent | FolderKind::Trash | FolderKind::Drafts
            ),
            Self::Unified | Self::Starred | Self::Account(_) | Self::Saved { .. } => true,
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
    /// The open saved search's own filter, ANDed under the live one.
    pub saved: Option<Filter>,
    /// Every saved search's filter, parsed, in config order: the sidebar's counts.
    pub saved_all: Vec<Filter>,
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
    /// The newest select-all read's sequence number: an older one skips its query. Its own
    /// gate, so a select-all and the list never cancel each other.
    latest_match: Arc<AtomicU64>,
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
            latest_match: Arc::default(),
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

    /// `read`, unless `gate` is given and a newer read of the same kind was asked for by the
    /// time the connection is free: then it answers `Stale` without querying. The repeated
    /// reads (the list, a select-all) each carry their own counter.
    async fn read_unless_stale<T: Send + 'static>(
        &self,
        gate: Option<(Arc<AtomicU64>, u64)>,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let (db, slot) = (self.db.clone(), self.conn.clone());
        #[cfg(test)]
        let queries = self.queries.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((latest, seq)) = &gate
                && *seq < latest.load(Ordering::SeqCst)
            {
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

    /// Record `seq` as the newest read of `counter`, so any older one still waiting skips
    /// itself.
    fn newest(counter: &AtomicU64, seq: u64) {
        counter.fetch_max(seq, Ordering::SeqCst);
    }

    /// The list read of `request`, or `Stale` when a newer one was asked for meanwhile.
    async fn read_list(&self, request: ListRequest) -> MailEvent {
        let seq = request.seq;
        let gate = Some((self.latest_list.clone(), seq));
        let result = self
            .read_unless_stale(gate, move |store| list(store, &request))
            .await;
        MailEvent::Listed { seq, result }
    }

    /// The threads `request` matches, at most `limit`, for a select-all; `Stale` when a newer
    /// select-all was asked for meanwhile. Gated because every `* a` runs a `LIMIT limit` scan
    /// on the one read connection the list, the sidebar and the reader share, and an answer a
    /// newer press has superseded is thrown away at the screen anyway.
    pub fn matching_targets(&self, request: ListRequest, limit: usize) -> Effect {
        Self::newest(&self.latest_match, request.seq);
        let reader = self.clone();
        job(move |sink| async move {
            let seq = request.seq;
            let gate = Some((reader.latest_match.clone(), seq));
            let result = reader
                .read_unless_stale(gate, move |store| matching(store, &request, limit))
                .await;
            send(&sink, MailEvent::Matched { seq, result });
        })
    }

    pub fn list(&self, request: ListRequest) -> Effect {
        Self::newest(&self.latest_list, request.seq);
        let reader = self.clone();
        job(move |sink| async move {
            send(&sink, reader.read_list(request).await);
        })
    }

    /// The `From` of this thread's newest message, for a select-by-sender. One row, so it goes
    /// through the ungated read like every other one-off.
    pub fn sender(&self, seq: u64, thread_id: i64) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.newest_sender(thread_id))
                .await;
            send(&sink, MailEvent::Sender { seq, result });
        })
    }

    /// The counts: each account's, and one per saved filter in `saved` (empty while the
    /// saved-search counts are off, so no count query runs). `names` resolves each saved
    /// query's own `account:`, as a list read does.
    pub fn sidebar(
        &self,
        seq: u64,
        addresses: Vec<String>,
        names: Vec<String>,
        saved: Vec<Filter>,
        utc_offset: i32,
    ) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| sidebar(store, &addresses, &names, &saved, utc_offset))
                .await;
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
        Self::newest(&self.latest_list, list.seq);
        let reader = self.clone();
        refresh_job(end, move |sink| async move {
            let (addresses, names) = (list.addresses.clone(), list.names.clone());
            let (saved, utc_offset) = (list.saved_all.clone(), list.utc_offset);
            send(&sink, reader.read_list(list).await);
            let result = reader
                .read(move |store| sidebar(store, &addresses, &names, &saved, utc_offset))
                .await;
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

    /// The move picker's folders for one account (by its folder model) and the ones `threads`
    /// are in now.
    pub fn move_choices(&self, seq: u64, account_id: i64, threads: Vec<i64>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader
                .read(move |store| store.move_choices(account_id, &threads))
                .await;
            send(&sink, MailEvent::Labels { seq, result });
        })
    }

    /// When the snoozes holding `threads` end, soonest first, in one read: a snooze chooser's
    /// title and its "Return to inbox now". Held as the Snoozed mailbox holds them, so a snooze
    /// with nothing left to bring back offers no return.
    pub fn snoozes(&self, seq: u64, threads: Vec<i64>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let result = reader.read(move |store| store.snooze_ends(&threads)).await;
            send(&sink, MailEvent::Snoozes { seq, result });
        })
    }

    /// The wake notice's subject: the thread's, when one came back.
    pub fn woke(&self, threads: Vec<i64>) -> Effect {
        let reader = self.clone();
        job(move |sink| async move {
            let count = threads.len();
            let subject = match threads.first().copied().filter(|_| count == 1) {
                Some(thread) => {
                    reader
                        .read(move |store| Ok(store.thread_detail(thread)?.map(|d| d.subject)))
                        .await
                }
                None => Ok(None),
            };
            send(&sink, MailEvent::WokeRead { count, subject });
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

/// Each configured account's store id and the sidebar's counts, including one unread count
/// per filter in `saved` (in config order). Each saved query's own `account:` is resolved
/// through `names`, as a list read resolves the live one, so a saved `account:home` search is
/// counted the way it is listed; with `saved` empty no count query runs (FR-19).
fn sidebar(
    store: &Store,
    addresses: &[String],
    names: &[String],
    saved: &[Filter],
    utc_offset: i32,
) -> Result<SidebarOutcome, StoreError> {
    let account_ids = store.account_ids(addresses)?;
    let known: Vec<i64> = account_ids.iter().flatten().copied().collect();
    let id_of = |index: usize| account_ids.get(index).copied().flatten();
    let filters: Vec<(Filter, AccountFilter)> = saved
        .iter()
        .map(|filter| {
            let account = account_filter(names, None, filter.account.as_deref(), id_of);
            (filter.clone(), account)
        })
        .collect();
    let counts = store.sidebar_counts(&known, &filters, utc_offset)?;
    Ok(SidebarOutcome {
        counts,
        account_ids,
    })
}

/// The store's scope for `scope`; `None` when its account has no row in the store yet.
fn store_scope(scope: &ScopeChoice, account_ids: &[Option<i64>]) -> Option<Scope> {
    let id_of = |index: usize| account_ids.get(index).copied().flatten();
    let all = || account_ids.iter().flatten().copied().collect::<Vec<i64>>();
    let kind = |kind| {
        Some(Scope::Kind {
            accounts: all(),
            kind,
        })
    };
    match scope {
        ScopeChoice::Unified => Some(Scope::Unified { accounts: all() }),
        ScopeChoice::Starred => Some(Scope::Starred { accounts: all() }),
        ScopeChoice::Snoozed => Some(Scope::Snoozed { accounts: all() }),
        ScopeChoice::Sent => kind(FolderKind::Sent),
        ScopeChoice::Trash => kind(FolderKind::Trash),
        // The Drafts pane reads its own rows; a list read of it lists the draft threads.
        ScopeChoice::Drafts => kind(FolderKind::Drafts),
        ScopeChoice::Account(index) => id_of(*index).map(|account_id| Scope::Inbox { account_id }),
        ScopeChoice::Folder {
            account, folder_id, ..
        } => id_of(*account).map(|account_id| Scope::Folder {
            account_id,
            folder_id: *folder_id,
        }),
        // A saved search is every folder of every account, narrowed by its own filter.
        ScopeChoice::Saved { .. } => Some(Scope::All { accounts: all() }),
    }
}

/// `name`'s store id, or `Nobody` when no configured account has that name (ignoring ASCII
/// case). `None` narrows nothing.
fn only(
    names: &[String],
    name: Option<&str>,
    id_of: &impl Fn(usize) -> Option<i64>,
) -> AccountFilter {
    match name {
        None => AccountFilter::Any,
        Some(name) => names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .and_then(id_of)
            .map_or(AccountFilter::Nobody, AccountFilter::Only),
    }
}

/// FR-13: the live filter's `account:` and the open saved search's, resolved through the same
/// `names` lookup and **intersected**. `Filter::message_sql` emits no account condition and
/// `ListQuery.account` is one `AccountFilter`, so the two must meet here: neither set is
/// `Any`, one set is its `Only(id)` (or `Nobody` for an unknown name), both set and equal is
/// that id, both set and different is `Nobody`. Any `Nobody` wins.
fn account_filter(
    names: &[String],
    live: Option<&str>,
    saved: Option<&str>,
    id_of: impl Fn(usize) -> Option<i64>,
) -> AccountFilter {
    match (only(names, live, &id_of), only(names, saved, &id_of)) {
        (AccountFilter::Nobody, _) | (_, AccountFilter::Nobody) => AccountFilter::Nobody,
        (AccountFilter::Any, other) | (other, AccountFilter::Any) => other,
        (AccountFilter::Only(a), AccountFilter::Only(b)) if a == b => AccountFilter::Only(a),
        (AccountFilter::Only(_), AccountFilter::Only(_)) => AccountFilter::Nobody,
    }
}

/// The one query a list read runs, scope and both `account:` terms resolved to store ids;
/// `None` when the scope's account has no row in the store yet. Everything a thread read
/// narrows by is built **here**, so anything else reading the same rows (a select-all over
/// what the list shows) inherits the saved filter instead of quietly dropping it.
fn list_query(request: &ListRequest, account_ids: &[Option<i64>]) -> Option<ListQuery> {
    // Fail closed. A `Saved` scope with no filter no longer names a saved search — the screen
    // could not find it — and `store_scope` would still widen it to every folder of every
    // account, with nothing left to narrow it: the whole mailbox under that search's title.
    // One guard here, so the list and a select-all both read nothing.
    if matches!(request.scope, ScopeChoice::Saved { .. }) && request.saved.is_none() {
        return None;
    }
    let id_of = |index: usize| account_ids.get(index).copied().flatten();
    let scope = store_scope(&request.scope, account_ids)?;
    let account = account_filter(
        &request.names,
        request.filter.account.as_deref(),
        request.saved.as_ref().and_then(|f| f.account.as_deref()),
        id_of,
    );
    Some(ListQuery {
        scope,
        filter: request.filter.clone(),
        saved: request.saved.clone(),
        account,
        utc_offset: request.utc_offset,
    })
}

/// The page `request` asks for, its scope and `account:` resolved to store ids. The ids the
/// outcome carries are the ones the query was built from: one lookup, not two.
fn list(store: &Store, request: &ListRequest) -> Result<ListOutcome, StoreError> {
    let account_ids = store.account_ids(&request.addresses)?;
    let page = match list_query(request, &account_ids) {
        Some(query) => store.thread_page(&query, request.at)?,
        // The account has no row in the store yet: nothing synced, nothing to list.
        None => ThreadPage::default(),
    };
    Ok(ListOutcome {
        page,
        at: request.at,
        account_ids,
    })
}

/// The threads `request` matches, at most `limit`, newest first: keys and flags only. Built on
/// the same `ListQuery` the list reads, so a select-all takes exactly the rows the list draws.
fn matching(
    store: &Store,
    request: &ListRequest,
    limit: usize,
) -> Result<Vec<(ThreadKey, i64, bool, bool)>, StoreError> {
    let account_ids = store.account_ids(&request.addresses)?;
    match list_query(request, &account_ids) {
        Some(query) => store.matching_targets(&query, limit),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use rail_core::store::filter::parse;

    use super::*;

    fn names() -> Vec<String> {
        vec!["work".to_string(), "home".to_string()]
    }

    /// Index 0 is store id 1, index 1 is store id 2.
    fn id_of(index: usize) -> Option<i64> {
        [Some(1), Some(2)].get(index).copied().flatten()
    }

    /// AC-13c (FR-13): the live `account:` and the saved one are resolved through the same
    /// name list and **intersected**; any unknown name is `Nobody`, and so is a disagreement.
    #[test]
    fn the_saved_and_live_account_filters_intersect() {
        let names = names();
        let filter =
            |live: Option<&str>, saved: Option<&str>| account_filter(&names, live, saved, id_of);
        assert_eq!(filter(None, None), AccountFilter::Any, "neither narrows");
        assert_eq!(filter(None, Some("home")), AccountFilter::Only(2));
        assert_eq!(filter(Some("home"), None), AccountFilter::Only(2));
        assert_eq!(
            filter(Some("HOME"), Some("home")),
            AccountFilter::Only(2),
            "names are compared ignoring case"
        );
        assert_eq!(
            filter(Some("home"), Some("work")),
            AccountFilter::Nobody,
            "two different accounts intersect to none"
        );
        assert_eq!(filter(Some("nope"), None), AccountFilter::Nobody);
        assert_eq!(filter(None, Some("nope")), AccountFilter::Nobody);
        assert_eq!(filter(Some("nope"), Some("home")), AccountFilter::Nobody);
    }

    /// Step 7 (FR-10): a saved search reads every folder of every account that has a store
    /// row, and the saved filter travels in the one `ListQuery` the builder makes.
    #[test]
    fn a_saved_search_scope_is_every_folder_of_every_account() {
        let ids = [Some(1), None, Some(3)];
        let scope = ScopeChoice::Saved {
            index: 0,
            name: "dana".into(),
        };
        assert_eq!(
            store_scope(&scope, &ids),
            Some(Scope::All {
                accounts: vec![1, 3]
            })
        );
    }

    /// FR-10: `ListRequest.saved` becomes `ListQuery.saved`, and the live filter stays its own.
    #[test]
    fn the_saved_filter_reaches_the_query() {
        let request = ListRequest {
            seq: 1,
            scope: ScopeChoice::Saved {
                index: 0,
                name: "dana".into(),
            },
            addresses: vec!["w@example.com".into()],
            names: names(),
            filter: parse("is:unread"),
            saved: Some(parse("from:dana")),
            saved_all: vec![parse("from:dana")],
            at: PageAt::First,
            utc_offset: 0,
        };
        let query = list_query(&request, &[Some(1)]).expect("the account has a store row");
        assert_eq!(query.scope, Scope::All { accounts: vec![1] });
        assert_eq!(query.saved.map(|f| f.from), Some(vec!["dana".to_string()]));
        assert!(query.filter.unread, "the live filter stays its own");
    }

    /// G2: a `Saved` scope whose filter did not come through builds no query at all, so both
    /// readers that go through `list_query` — the page read and the select-all's match read —
    /// see nothing. Without the guard `store_scope` still answers `Scope::All`, and with an
    /// empty live filter that query is every thread of every account.
    #[test]
    fn a_saved_scope_with_no_filter_builds_no_query() {
        let request = ListRequest {
            seq: 1,
            scope: ScopeChoice::Saved {
                index: 7,
                name: "gone".into(),
            },
            addresses: vec!["w@example.com".into()],
            names: names(),
            filter: parse(""),
            saved: None,
            saved_all: Vec::new(),
            at: PageAt::First,
            utc_offset: 0,
        };
        assert_eq!(
            store_scope(&request.scope, &[Some(1)]),
            Some(Scope::All { accounts: vec![1] }),
            "the scope on its own is still every folder"
        );
        assert!(list_query(&request, &[Some(1)]).is_none());
    }
}
