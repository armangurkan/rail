//! A scripted provider for engine tests: an in-memory mailbox with a history log, a page
//! size, injected failures, a message that arrives mid-backfill, and a record of calls.
//! Windows compare in whole seconds, like Gmail's `after:` and `before:`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::{
    Body, ChangeSet, Envelope, EnvelopePage, PageToken, Profile, Provider, ProviderError, Relabel,
    RemoteFolder, RemoteId, SyncCursor, Window,
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
}

pub(crate) enum Change {
    Added(String),
    Deleted(String),
    Relabel(Relabel),
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

fn arrive(m: &mut Mailbox, envelope: Envelope) {
    let id = envelope.remote_id.clone();
    m.messages.insert(id.clone(), envelope);
    record(m, Change::Added(id));
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

    async fn backfill(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<EnvelopePage, ProviderError> {
        self.enter(Call::Backfill, Some(*window))?;
        let delay = self.lock().page_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        let offset: usize = page.map_or(0, |p| p.0.parse().unwrap());
        let mut listed: Vec<Envelope> = m
            .messages
            .values()
            .filter(|e| window.contains(e.internal_date))
            .cloned()
            .collect();
        listed.sort_by_key(|e| std::cmp::Reverse(e.internal_date));
        let size = m.page_size.max(1);
        let chunk: Vec<Envelope> = listed.iter().skip(offset).take(size).cloned().collect();
        let next = (offset + size < listed.len()).then(|| PageToken((offset + size).to_string()));
        let (envelopes, skipped) = readable(&m, chunk);
        m.pages_served += 1;
        if m.arrive_after_page
            .as_ref()
            .is_some_and(|(n, _)| *n == m.pages_served)
        {
            let (_, newcomer) = m.arrive_after_page.take().unwrap();
            arrive(&mut m, newcomer);
        }
        Ok(EnvelopePage {
            envelopes,
            skipped,
            next,
        })
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
        let m = self.lock();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_mock_pages_newest_first_and_plays_history() {
        let mock = MockProvider::new();
        mock.with(|m| m.page_size = 2);
        for (i, ms) in [1_000, 3_000, 2_000].into_iter().enumerate() {
            mock.seed(envelope(&format!("m{i}"), "t", ms));
        }
        let first = mock.backfill(&Window::default(), None).await.unwrap();
        let ids: Vec<_> = first
            .envelopes
            .iter()
            .map(|e| e.remote_id.as_str())
            .collect();
        assert_eq!(ids, ["m1", "m2"]);
        let second = mock
            .backfill(&Window::default(), first.next.as_ref())
            .await
            .unwrap();
        assert_eq!(second.envelopes[0].remote_id, "m0");
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
        let page = mock.backfill(&window, None).await.unwrap();
        assert!(
            page.envelopes.is_empty(),
            "before:5 excludes all of second 5"
        );
        mock.fail_next(Call::Folders, ProviderError::transient("down"));
        assert!(mock.folders().await.is_err());
        assert!(mock.folders().await.is_ok());
    }
}
