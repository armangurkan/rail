//! A scripted provider for engine tests: an in-memory mailbox with a history log, a page
//! size, injected failures, a message that arrives mid-backfill, and a record of calls.
//! Windows compare in whole seconds, like Gmail's `after:` and `before:`. Like Gmail, it can
//! list out of order (`shuffle_within_ms`), and its page tokens are valid only for their own
//! listing and refused once stale (`expire_page_tokens`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::http::{RetryPolicy, retry_policy_override};
use super::{
    Body, ChangeSet, Envelope, EnvelopePage, IdPage, PageToken, Profile, Provider, ProviderError,
    Relabel, RemoteFolder, RemoteId, SentReceipt, SyncCursor, Window,
};
use crate::provider::FolderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    Folders,
    Profile,
    Backfill,
    Changes,
    Body,
    Search,
    Envelopes,
    Modify,
    Trash,
    Send,
    FindByHeader,
    Attachment,
}

pub(crate) enum Change {
    Added(String),
    Deleted(String),
    Relabel(Relabel),
}

/// One message the mock was asked to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentRaw {
    pub raw: Vec<u8>,
    pub thread_id: Option<String>,
}

pub(crate) struct Mailbox {
    pub folders: Vec<RemoteFolder>,
    pub messages: BTreeMap<String, Envelope>,
    pub bodies: HashMap<String, Body>,
    pub history: Vec<(u64, Change)>,
    pub history_id: u64,
    /// Cursors below this are expired.
    pub expired_below: u64,
    pub page_size: usize,
    pub search_hits: Vec<String>,
    /// Ids whose metadata or body "fails to parse".
    pub broken: HashSet<String>,
    /// A failure for a call, after that many successful calls of it.
    pub failures: VecDeque<(Call, usize, ProviderError)>,
    pub calls: Vec<(Call, Option<Window>)>,
    /// After serving this many backfill pages, this message arrives (with history).
    pub arrive_after_page: Option<(usize, Envelope)>,
    pub pages_served: usize,
    /// How long each backfill page takes.
    pub page_delay: Duration,
    pub body_delay: Duration,
    pub bodies_in_flight: usize,
    pub max_bodies_in_flight: usize,
    /// History records one `changes` answer carries at most (0: all); `more` says the rest wait.
    pub change_page: usize,
    /// Every `changes` answer says more waits, as a broken server might.
    pub always_more: bool,
    /// List out of order, as Gmail does: each message sorts as if received up to this many
    /// milliseconds later than it was (a fixed amount per id). 0: strictly newest first.
    pub shuffle_within_ms: i64,
    /// Page tokens carry the generation they were issued in; older ones are refused, as
    /// Gmail refuses a stale token (`expire_page_tokens`).
    pub token_generation: u64,
    /// Answer a stale or foreign token with the listing's first page instead of refusing it,
    /// as Gmail answered a made-up token in the live test.
    pub ignore_unknown_tokens: bool,
    /// Ids listed before every other one, in this order, whatever their date (Gmail lists
    /// imported mail by when it arrived, not by its `internalDate`).
    pub list_first: Vec<String>,
    /// The page token each listing call was given, in order (failed calls too).
    pub list_tokens: Vec<Option<String>>,
    /// Every id whose metadata was asked for (`envelopes`), in order: Gmail's `messages.get`.
    pub metadata_gets: Vec<String>,
    /// The scoped retry policy each call ran under (`None`: the client's own), in order.
    pub policies: Vec<(Call, Option<RetryPolicy>)>,
    /// Every `modify_threads` call: (thread ids, add, remove), in order.
    pub modifies: Vec<(Vec<RemoteId>, Vec<RemoteId>, Vec<RemoteId>)>,
    /// Every thread id `trash_threads` was given, in order.
    pub trashed: Vec<RemoteId>,
    /// How long each modify or trash call takes.
    pub action_delay: Duration,
    /// Every `send` call, in order.
    pub sent: Vec<SentRaw>,
    /// `Message-ID` headers the mailbox holds (`<id>`): a send adds its own.
    pub known_header_ids: HashSet<String>,
    /// Attachment bytes by (message id, part id).
    pub attachment_bytes: HashMap<(String, String), Vec<u8>>,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            folders: vec![RemoteFolder {
                remote_id: "INBOX".into(),
                name: "Inbox".into(),
                kind: FolderKind::Inbox,
            }],
            messages: BTreeMap::new(),
            bodies: HashMap::new(),
            history: Vec::new(),
            history_id: 1,
            expired_below: 0,
            page_size: 100,
            search_hits: Vec::new(),
            broken: HashSet::new(),
            failures: VecDeque::new(),
            calls: Vec::new(),
            arrive_after_page: None,
            pages_served: 0,
            page_delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            bodies_in_flight: 0,
            max_bodies_in_flight: 0,
            change_page: 0,
            always_more: false,
            shuffle_within_ms: 0,
            token_generation: 0,
            ignore_unknown_tokens: false,
            list_first: Vec::new(),
            list_tokens: Vec::new(),
            metadata_gets: Vec::new(),
            policies: Vec::new(),
            modifies: Vec::new(),
            trashed: Vec::new(),
            action_delay: Duration::ZERO,
            sent: Vec::new(),
            known_header_ids: HashSet::new(),
            attachment_bytes: HashMap::new(),
        }
    }
}

/// A test message: subject `subject <id>`, from `sender@example.com`, in the inbox, unread.
pub(crate) fn envelope(id: &str, thread: &str, internal_ms: i64) -> Envelope {
    Envelope {
        remote_id: id.to_string(),
        thread_id: thread.to_string(),
        subject: format!("subject {id}"),
        from: vec![super::Mailbox {
            name: Some("Sender".into()),
            address: "sender@example.com".into(),
        }],
        date: internal_ms / 1000,
        internal_date: internal_ms,
        snippet: format!("snippet {id}"),
        folders: vec!["INBOX".into()],
        ..Envelope::default()
    }
}

#[derive(Clone, Default)]
pub(crate) struct MockProvider {
    pub mailbox: Arc<Mutex<Mailbox>>,
}

impl MockProvider {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Mailbox> {
        self.mailbox.lock().unwrap()
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Mailbox) -> R) -> R {
        f(&mut self.lock())
    }

    /// A message that was there before any sync: no history record.
    pub(crate) fn seed(&self, envelope: Envelope) {
        self.lock()
            .messages
            .insert(envelope.remote_id.clone(), envelope);
    }

    /// A message that arrives now, with a history record.
    pub(crate) fn add(&self, envelope: Envelope) {
        arrive(&mut self.lock(), envelope);
    }

    pub(crate) fn delete(&self, id: &str) {
        let mut m = self.lock();
        m.messages.remove(id);
        record(&mut m, Change::Deleted(id.to_string()));
    }

    pub(crate) fn relabel(&self, change: Relabel) {
        let mut m = self.lock();
        if let Some(env) = m.messages.get_mut(&change.remote_id) {
            env.folders.retain(|f| !change.remove_folders.contains(f));
            env.folders.extend(change.add_folders.iter().cloned());
            env.is_read = change.read.unwrap_or(env.is_read);
            env.is_starred = change.starred.unwrap_or(env.is_starred);
        }
        record(&mut m, Change::Relabel(change));
    }

    pub(crate) fn fail_next(&self, call: Call, error: ProviderError) {
        self.fail_after(call, 0, error);
    }

    /// Fail the call after `successes` more successful calls of it.
    pub(crate) fn fail_after(&self, call: Call, successes: usize, error: ProviderError) {
        self.lock().failures.push_back((call, successes, error));
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.lock().calls.iter().map(|(c, _)| *c).collect()
    }

    /// Refuse every page token issued so far.
    pub(crate) fn expire_page_tokens(&self) {
        self.lock().token_generation += 1;
    }

    /// How many times each id's metadata was asked for, for the ids asked more than once.
    pub(crate) fn metadata_refetches(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for id in &self.lock().metadata_gets {
            *counts.entry(id.clone()).or_default() += 1;
        }
        counts.into_iter().filter(|(_, n)| *n > 1).collect()
    }

    pub(crate) fn backfill_windows(&self) -> Vec<Window> {
        self.lock()
            .calls
            .iter()
            .filter_map(|(c, w)| (*c == Call::Backfill).then_some(*w).flatten())
            .collect()
    }

    /// Record the call and fail it if a failure for it is queued.
    fn enter(&self, call: Call, window: Option<Window>) -> Result<(), ProviderError> {
        let mut m = self.lock();
        m.calls.push((call, window));
        m.policies.push((call, retry_policy_override()));
        let Some(index) = m.failures.iter().position(|(c, _, _)| *c == call) else {
            return Ok(());
        };
        if m.failures[index].1 > 0 {
            m.failures[index].1 -= 1;
            return Ok(());
        }
        Err(m.failures.remove(index).map(|(_, _, e)| e).unwrap())
    }
}

fn record(m: &mut Mailbox, change: Change) {
    m.history_id += 1;
    let id = m.history_id;
    m.history.push((id, change));
}

/// Apply a thread change to the mock's messages, with a history record each, as Gmail would.
fn relabel_threads(m: &mut Mailbox, ids: &[RemoteId], add: &[RemoteId], remove: &[RemoteId]) {
    use super::{STARRED_LABEL, UNREAD_LABEL};
    let flag = |labels: &[RemoteId], name: &str| labels.iter().any(|l| l == name);
    let folders = |labels: &[RemoteId]| -> Vec<RemoteId> {
        labels
            .iter()
            .filter(|l| *l != UNREAD_LABEL && *l != STARRED_LABEL)
            .cloned()
            .collect()
    };
    let changed: Vec<String> = m
        .messages
        .values()
        .filter(|e| ids.contains(&e.thread_id))
        .map(|e| e.remote_id.clone())
        .collect();
    for id in changed {
        let change = Relabel {
            remote_id: id.clone(),
            add_folders: folders(add),
            remove_folders: folders(remove),
            read: flag(remove, UNREAD_LABEL)
                .then_some(true)
                .or(flag(add, UNREAD_LABEL).then_some(false)),
            starred: flag(add, STARRED_LABEL)
                .then_some(true)
                .or(flag(remove, STARRED_LABEL).then_some(false)),
        };
        if let Some(env) = m.messages.get_mut(&id) {
            env.folders.retain(|f| !change.remove_folders.contains(f));
            env.folders.extend(change.add_folders.iter().cloned());
            env.is_read = change.read.unwrap_or(env.is_read);
            env.is_starred = change.starred.unwrap_or(env.is_starred);
        }
        record(m, Change::Relabel(change));
    }
}

fn arrive(m: &mut Mailbox, envelope: Envelope) {
    let id = envelope.remote_id.clone();
    m.messages.insert(id.clone(), envelope);
    record(m, Change::Added(id));
}

/// A page token that says where the next page starts, for which listing, in which generation.
fn issue_token(generation: u64, window: &Window, offset: usize) -> PageToken {
    let bound = |b: Option<i64>| b.map_or("-".to_string(), |v| v.to_string());
    PageToken(format!(
        "{generation}.{}.{}.{offset}",
        bound(window.after_ms),
        bound(window.before_ms)
    ))
}

/// Where `token` says the next page starts; refused when it is stale or from another listing
/// (or, with `ignore_unknown_tokens`, read as the start).
fn token_offset(m: &Mailbox, window: &Window, token: &PageToken) -> Result<usize, ProviderError> {
    let offset = token.0.rsplit('.').next().and_then(|o| o.parse().ok());
    match offset {
        Some(offset) if issue_token(m.token_generation, window, offset) == *token => Ok(offset),
        _ if m.ignore_unknown_tokens => Ok(0),
        _ => Err(ProviderError::PageTokenRejected),
    }
}

/// Where a message sorts in a listing: its receive time, pushed later by a fixed amount below
/// `spread` (FNV-1a of its id), so a listing is out of order by up to `spread` ms.
fn listing_key(envelope: &Envelope, spread: i64) -> (i64, String) {
    let jitter = match u64::try_from(spread) {
        Ok(spread) if spread > 0 => {
            let hash = envelope
                .remote_id
                .bytes()
                .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                });
            i64::try_from(hash % spread).unwrap()
        }
        _ => 0,
    };
    (envelope.internal_date + jitter, envelope.remote_id.clone())
}

/// Split envelopes into readable ones and the ids of "broken" ones.
fn readable(m: &Mailbox, envelopes: Vec<Envelope>) -> (Vec<Envelope>, Vec<RemoteId>) {
    let (broken, fine): (Vec<_>, Vec<_>) = envelopes
        .into_iter()
        .partition(|e| m.broken.contains(&e.remote_id));
    (fine, broken.into_iter().map(|e| e.remote_id).collect())
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn folders(&self) -> Result<Vec<RemoteFolder>, ProviderError> {
        self.enter(Call::Folders, None)?;
        Ok(self.lock().folders.clone())
    }

    async fn profile(&self) -> Result<Profile, ProviderError> {
        self.enter(Call::Profile, None)?;
        let m = self.lock();
        Ok(Profile {
            cursor: SyncCursor(m.history_id.to_string()),
            messages_total: m.messages.len() as u64,
        })
    }

    async fn list(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<IdPage, ProviderError> {
        // Recorded before `enter`, so `list_tokens` lines up with `backfill_windows`.
        self.lock().list_tokens.push(page.map(|p| p.0.clone()));
        self.enter(Call::Backfill, Some(*window))?;
        let delay = self.lock().page_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        let offset = match page {
            None => 0,
            Some(token) => token_offset(&m, window, token)?,
        };
        let mut listed: Vec<&Envelope> = m
            .messages
            .values()
            .filter(|e| window.contains(e.internal_date))
            .collect();
        let spread = m.shuffle_within_ms;
        listed.sort_by_key(|e| {
            let first = m.list_first.iter().position(|id| *id == e.remote_id);
            (
                first.unwrap_or(usize::MAX),
                std::cmp::Reverse(listing_key(e, spread)),
            )
        });
        let size = m.page_size.max(1);
        let ids: Vec<RemoteId> = listed
            .iter()
            .skip(offset)
            .take(size)
            .map(|e| e.remote_id.clone())
            .collect();
        let next = (offset + size < listed.len())
            .then(|| issue_token(m.token_generation, window, offset + size));
        m.pages_served += 1;
        if m.arrive_after_page
            .as_ref()
            .is_some_and(|(n, _)| *n == m.pages_served)
        {
            let (_, newcomer) = m.arrive_after_page.take().unwrap();
            arrive(&mut m, newcomer);
        }
        Ok(IdPage { ids, next })
    }

    fn list_page_size(&self) -> usize {
        self.lock().page_size.max(1)
    }

    async fn changes(&self, cursor: &SyncCursor) -> Result<ChangeSet, ProviderError> {
        self.enter(Call::Changes, None)?;
        let m = self.lock();
        let from: u64 = cursor.0.parse().unwrap();
        if from < m.expired_below {
            return Err(ProviderError::CursorExpired);
        }
        let pending: Vec<&(u64, Change)> = m.history.iter().filter(|(id, _)| *id > from).collect();
        let take = match m.change_page {
            0 => pending.len(),
            page => page.min(pending.len()),
        };
        let cursor = match take < pending.len() {
            true => pending[take - 1].0,
            false => m.history_id,
        };
        let mut set = ChangeSet {
            cursor: SyncCursor(cursor.to_string()),
            more: take < pending.len() || m.always_more,
            ..ChangeSet::default()
        };
        for (_, change) in pending.into_iter().take(take) {
            match change {
                Change::Added(id) if m.broken.contains(id) => set.skipped.push(id.clone()),
                Change::Added(id) => set.added.extend(m.messages.get(id).cloned()),
                Change::Deleted(id) => set.deleted.push(id.clone()),
                Change::Relabel(r) => set.relabeled.push(r.clone()),
            }
        }
        Ok(set)
    }

    async fn body(&self, id: &str) -> Result<Body, ProviderError> {
        self.enter(Call::Body, None)?;
        let delay = {
            let mut m = self.lock();
            m.bodies_in_flight += 1;
            m.max_bodies_in_flight = m.max_bodies_in_flight.max(m.bodies_in_flight);
            m.body_delay
        };
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        m.bodies_in_flight -= 1;
        if m.broken.contains(id) {
            return Err(ProviderError::permanent("could not read the body"));
        }
        Ok(m.bodies.get(id).cloned().unwrap_or_else(|| Body {
            text: format!("body of {id}"),
            ..Body::default()
        }))
    }

    async fn search(&self, _query: &str, limit: usize) -> Result<Vec<RemoteId>, ProviderError> {
        self.enter(Call::Search, None)?;
        Ok(self
            .lock()
            .search_hits
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn envelopes(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError> {
        self.enter(Call::Envelopes, None)?;
        let mut m = self.lock();
        m.metadata_gets.extend(ids.iter().cloned());
        let found: Vec<Envelope> = ids
            .iter()
            .filter_map(|id| m.messages.get(id).cloned())
            .collect();
        let (envelopes, skipped) = readable(&m, found);
        Ok(EnvelopePage {
            envelopes,
            skipped,
            next: None,
        })
    }

    async fn modify_threads(
        &self,
        ids: &[RemoteId],
        add: &[RemoteId],
        remove: &[RemoteId],
    ) -> Result<(), ProviderError> {
        self.enter(Call::Modify, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        m.modifies
            .push((ids.to_vec(), add.to_vec(), remove.to_vec()));
        relabel_threads(&mut m, ids, add, remove);
        Ok(())
    }

    async fn trash_threads(&self, ids: &[RemoteId]) -> Result<(), ProviderError> {
        self.enter(Call::Trash, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        m.trashed.extend(ids.iter().cloned());
        relabel_threads(&mut m, ids, &["TRASH".into()], &[super::INBOX_LABEL.into()]);
        Ok(())
    }

    async fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SentReceipt, ProviderError> {
        self.enter(Call::Send, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        m.sent.push(SentRaw {
            raw: raw.to_vec(),
            thread_id: thread_id.map(str::to_string),
        });
        if let Some(id) = header_message_id(raw) {
            m.known_header_ids.insert(id);
        }
        let n = m.sent.len();
        Ok(SentReceipt {
            message_id: Some(format!("sent-{n}")),
            thread_id: Some(thread_id.map_or_else(|| format!("t-sent-{n}"), str::to_string)),
        })
    }

    async fn message_by_header_id(
        &self,
        rfc822_message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        self.enter(Call::FindByHeader, None)?;
        let m = self.lock();
        Ok(m.known_header_ids
            .contains(rfc822_message_id)
            .then(|| format!("found-{rfc822_message_id}")))
    }

    async fn attachment(&self, message: &RemoteId, part: &str) -> Result<Vec<u8>, ProviderError> {
        self.enter(Call::Attachment, None)?;
        let m = self.lock();
        m.attachment_bytes
            .get(&(message.clone(), part.to_string()))
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
}

/// The `Message-ID` of raw message bytes (angle brackets kept), found by a line scan of the
/// header block; the mock is compiled without a parser.
fn header_message_id(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        if line.is_empty() {
            return None;
        }
        if let (Some(name), Some(value)) = (line.get(..11), line.get(11..))
            && name.eq_ignore_ascii_case("message-id:")
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_sent_message_is_found_afterwards_by_its_header_id() {
        let mock = MockProvider::new();
        let raw = b"From: a@example.com\r\nMessage-ID: <abc@example.com>\r\n\r\nHi";
        assert_eq!(
            mock.message_by_header_id("<abc@example.com>")
                .await
                .unwrap(),
            None
        );
        let receipt = mock.send(raw, None).await.unwrap();
        assert_eq!(receipt.message_id.as_deref(), Some("sent-1"));
        assert_eq!(receipt.thread_id.as_deref(), Some("t-sent-1"));
        assert_eq!(
            mock.message_by_header_id("<abc@example.com>")
                .await
                .unwrap(),
            Some("found-<abc@example.com>".to_string())
        );
        let again = mock.send(raw, Some("t9")).await.unwrap();
        assert_eq!(
            (
                again.message_id.as_deref().unwrap(),
                again.thread_id.as_deref().unwrap()
            ),
            ("sent-2", "t9")
        );
        let calls: Vec<Call> = mock.with(|m| m.calls.iter().map(|(c, _)| *c).collect());
        assert_eq!(
            calls,
            [
                Call::FindByHeader,
                Call::Send,
                Call::FindByHeader,
                Call::Send
            ]
        );
        assert_eq!(
            mock.with(|m| m.sent[1].thread_id.clone()),
            Some("t9".to_string())
        );
    }

    #[tokio::test]
    async fn the_mock_serves_attachment_bytes_or_not_found() {
        let mock = MockProvider::new();
        mock.with(|m| {
            m.attachment_bytes
                .insert(("m1".into(), "p1".into()), vec![1, 2])
        });
        assert_eq!(
            mock.attachment(&"m1".to_string(), "p1").await.unwrap(),
            [1, 2]
        );
        assert!(matches!(
            mock.attachment(&"m1".to_string(), "p2").await,
            Err(ProviderError::NotFound)
        ));
    }

    #[tokio::test]
    async fn the_mock_records_thread_changes_and_plays_them_as_history() {
        let mock = MockProvider::new();
        mock.seed(envelope("m1", "t1", 1_000));
        let cursor = SyncCursor(mock.with(|m| m.history_id).to_string());
        mock.modify_threads(
            &["t1".into()],
            &["STARRED".into()],
            &["INBOX".into(), "UNREAD".into()],
        )
        .await
        .unwrap();
        mock.trash_threads(&["t1".into()]).await.unwrap();
        let calls: Vec<Call> = mock.with(|m| m.calls.iter().map(|(c, _)| *c).collect());
        assert_eq!(calls, [Call::Modify, Call::Trash]);
        let env = mock.with(|m| m.messages["m1"].clone());
        assert!(env.is_starred && env.is_read);
        assert_eq!(env.folders, ["TRASH"]);
        let changes = mock.changes(&cursor).await.unwrap();
        assert_eq!(changes.relabeled.len(), 2);
        mock.fail_next(Call::Trash, ProviderError::NotFound);
        assert_eq!(
            mock.trash_threads(&["t1".into()]).await,
            Err(ProviderError::NotFound)
        );
    }

    #[tokio::test]
    async fn the_mock_pages_newest_first_and_plays_history() {
        let mock = MockProvider::new();
        mock.with(|m| m.page_size = 2);
        for (i, ms) in [1_000, 3_000, 2_000].into_iter().enumerate() {
            mock.seed(envelope(&format!("m{i}"), "t", ms));
        }
        let first = mock.list(&Window::default(), None).await.unwrap();
        assert_eq!(first.ids, ["m1", "m2"]);
        let second = mock
            .list(&Window::default(), first.next.as_ref())
            .await
            .unwrap();
        assert_eq!(second.ids, ["m0"]);
        assert!(second.next.is_none());
        let start = mock.profile().await.unwrap().cursor;
        mock.add(envelope("new", "t2", 9_000));
        mock.delete("m0");
        let read = Relabel {
            remote_id: "m1".into(),
            add_folders: vec!["STARRED-ish".into()],
            remove_folders: vec!["INBOX".into()],
            read: Some(true),
            starred: None,
        };
        mock.relabel(read.clone());
        let m1 = mock.with(|m| m.messages["m1"].clone());
        assert_eq!(m1.folders, ["STARRED-ish"]);
        assert!(m1.is_read && !m1.is_starred);
        let changes = mock.changes(&start).await.unwrap();
        assert_eq!(changes.added[0].remote_id, "new");
        assert_eq!(changes.deleted, ["m0"]);
        assert_eq!(changes.relabeled, [read]);
        assert!(!changes.more);
        assert_eq!(
            mock.calls(),
            [Call::Backfill, Call::Backfill, Call::Profile, Call::Changes]
        );
        assert_eq!(
            mock.backfill_windows(),
            [Window::default(), Window::default()]
        );
        mock.with(|m| m.expired_below = u64::MAX);
        assert_eq!(
            mock.changes(&start).await.unwrap_err(),
            ProviderError::CursorExpired
        );
    }

    #[tokio::test]
    async fn the_mock_windows_in_whole_seconds_like_gmail() {
        let mock = MockProvider::new();
        mock.seed(envelope("a", "t", 5_000));
        mock.seed(envelope("b", "t", 5_900));
        let window = Window {
            after_ms: None,
            before_ms: Some(5_500),
        };
        let page = mock.list(&window, None).await.unwrap();
        assert!(page.ids.is_empty(), "before:5 excludes all of second 5");
        mock.fail_next(Call::Folders, ProviderError::transient("down"));
        assert!(mock.folders().await.is_err());
        assert!(mock.folders().await.is_ok());
    }

    /// The mock's tokens behave as the engine must assume Gmail's do: valid only for their
    /// own listing, refused once stale; and a shuffled listing is out of order.
    #[tokio::test]
    async fn the_mock_refuses_stale_or_foreign_tokens_and_can_shuffle() {
        let mock = MockProvider::new();
        mock.with(|m| m.page_size = 1);
        for n in 0..10 {
            mock.seed(envelope(&format!("m{n}"), "t", 10_000 * (n + 1)));
        }
        let first = mock.list(&Window::default(), None).await.unwrap();
        let token = first.next.unwrap();
        let other = Window {
            after_ms: Some(1),
            before_ms: None,
        };
        assert_eq!(
            mock.list(&other, Some(&token)).await.unwrap_err(),
            ProviderError::PageTokenRejected
        );
        assert!(mock.list(&Window::default(), Some(&token)).await.is_ok());
        mock.expire_page_tokens();
        assert_eq!(
            mock.list(&Window::default(), Some(&token))
                .await
                .unwrap_err(),
            ProviderError::PageTokenRejected
        );
        mock.with(|m| {
            m.page_size = 10;
            m.shuffle_within_ms = 1_000_000;
        });
        let all = mock.list(&Window::default(), None).await.unwrap();
        let dates: Vec<i64> = all
            .ids
            .iter()
            .map(|id| mock.with(|m| m.messages[id].internal_date))
            .collect();
        assert!(dates.windows(2).any(|w| w[0] < w[1]), "{dates:?}");
        let page = mock.envelopes(&all.ids[..2]).await.unwrap();
        assert_eq!(page.envelopes.len(), 2);
        assert_eq!(mock.with(|m| m.metadata_gets.len()), 2);
    }
}
