//! Operations waiting on a human: one entry per begun login or consent, from `Waiting` through
//! `Saving` to a finished state. Finished entries stay for ten minutes so a late `pending.wait`
//! still gets the answer.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};

use crate::ops::{PendingId, PendingKind, PendingState, WaitResponse};

/// How long a finished entry is kept for `pending.wait`.
pub const RETAIN_FINISHED: Duration = Duration::from_secs(600);
const SAVE_POLL: Duration = Duration::from_millis(50);

struct Entry {
    kind: PendingKind,
    state: PendingState,
    /// Taken by `cancel`; dropping it without sending also ends the wait.
    cancel: Option<oneshot::Sender<()>>,
    finished_at: Option<Instant>,
}

#[derive(Default)]
struct Entries {
    next: u64,
    by_id: HashMap<PendingId, Entry>,
}

impl Entries {
    /// Drop finished entries older than `RETAIN_FINISHED`, so the table stays bounded.
    fn expire(&mut self) {
        self.expire_after(RETAIN_FINISHED);
    }

    fn expire_after(&mut self, retain: Duration) {
        self.by_id
            .retain(|_, e| e.finished_at.is_none_or(|at| at.elapsed() < retain));
    }
}

pub(crate) struct PendingTable {
    entries: Mutex<Entries>,
    /// Bumped on every change, so waiters wake without polling.
    changed: watch::Sender<u64>,
}

impl Default for PendingTable {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            changed: watch::Sender::new(0),
        }
    }
}

fn prefix(kind: PendingKind) -> &'static str {
    match kind {
        PendingKind::AddAccount => "add",
        PendingKind::Login => "login",
        PendingKind::AdminConsent => "consent",
    }
}

impl PendingTable {
    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// A new waiting entry, and the receiver its task watches for a cancel.
    pub(crate) fn begin(&self, kind: PendingKind) -> (PendingId, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        let mut entries = self.lock();
        entries.expire();
        entries.next += 1;
        let id = PendingId(format!("{}-{}", prefix(kind), entries.next));
        let entry = Entry {
            kind,
            state: PendingState::Waiting,
            cancel: Some(tx),
            finished_at: None,
        };
        entries.by_id.insert(id.clone(), entry);
        drop(entries);
        self.bump();
        (id, rx)
    }

    pub(crate) fn get(&self, id: &PendingId) -> Option<WaitResponse> {
        let mut entries = self.lock();
        entries.expire();
        entries.by_id.get(id).map(|e| WaitResponse {
            id: id.clone(),
            kind: e.kind,
            state: e.state.clone(),
        })
    }

    /// Waiting or saving: the daemon must not idle out while any exist.
    pub(crate) fn active(&self) -> usize {
        self.lock()
            .by_id
            .values()
            .filter(|e| !e.state.is_finished())
            .count()
    }

    fn saving(&self) -> usize {
        self.lock()
            .by_id
            .values()
            .filter(|e| e.state == PendingState::Saving)
            .count()
    }

    /// `Waiting` to `Saving`. False when the entry was cancelled first: then nothing is saved.
    pub(crate) fn start_saving(&self, id: &PendingId) -> bool {
        let mut entries = self.lock();
        let Some(entry) = entries.by_id.get_mut(id) else {
            return false;
        };
        if entry.state != PendingState::Waiting {
            return false;
        }
        entry.state = PendingState::Saving;
        entry.cancel = None;
        drop(entries);
        self.bump();
        true
    }

    /// Record the outcome unless the entry already finished (it was cancelled).
    pub(crate) fn finish(&self, id: &PendingId, state: PendingState) -> Option<WaitResponse> {
        let mut entries = self.lock();
        let entry = entries.by_id.get_mut(id)?;
        if entry.state.is_finished() {
            return None;
        }
        entry.state = state;
        entry.cancel = None;
        entry.finished_at = Some(Instant::now());
        drop(entries);
        self.bump();
        self.get(id)
    }

    /// Cancel a waiting entry. A save in progress, or a finished entry, is left as it is; the
    /// answer says which.
    pub(crate) fn cancel(&self, id: &PendingId) -> Option<WaitResponse> {
        let mut entries = self.lock();
        let entry = entries.by_id.get_mut(id)?;
        if entry.state == PendingState::Waiting {
            if let Some(cancel) = entry.cancel.take()
                && cancel.send(()).is_err()
            {
                tracing::warn!(id = %id, "a cancelled login's task was already gone");
            }
            entry.state = PendingState::Cancelled;
            entry.finished_at = Some(Instant::now());
        }
        drop(entries);
        self.bump();
        self.get(id)
    }

    /// The entry once it finishes, or as it stands after `timeout`. `None` for an unknown id.
    pub(crate) async fn wait(&self, id: &PendingId, timeout: Duration) -> Option<WaitResponse> {
        let mut changes = self.changed.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = self.get(id)?;
            if current.state.is_finished() {
                return Some(current);
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return self.get(id),
            }
        }
    }

    /// Cancel every entry still waiting for a browser, closing their loopback listeners. A
    /// save in progress is left alone. Used at shutdown, before waiting for saves.
    pub(crate) fn cancel_waiting(&self) {
        let waiting: Vec<PendingId> = self
            .lock()
            .by_id
            .iter()
            .filter(|(_, e)| e.state == PendingState::Waiting)
            .map(|(id, _)| id.clone())
            .collect();
        for id in waiting {
            self.cancel(&id);
        }
    }

    /// Wait, at most `limit`, until no save is running.
    pub(crate) async fn wait_for_saves(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        while self.saving() > 0 && Instant::now() < deadline {
            tokio::time::sleep(SAVE_POLL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OpError, PendingOutcome};

    #[test]
    fn ids_name_their_kind_and_never_repeat() {
        let table = PendingTable::default();
        let (a, _) = table.begin(PendingKind::AddAccount);
        let (b, _) = table.begin(PendingKind::Login);
        let (c, _) = table.begin(PendingKind::AdminConsent);
        assert_eq!([a.0, b.0, c.0], ["add-1", "login-2", "consent-3"]);
        assert_eq!(table.active(), 3);
    }

    #[test]
    fn cancel_ends_a_waiting_entry_and_wakes_its_task() {
        let table = PendingTable::default();
        let (id, mut rx) = table.begin(PendingKind::Login);
        assert_eq!(table.cancel(&id).unwrap().state, PendingState::Cancelled);
        assert!(rx.try_recv().is_ok());
        assert!(!table.start_saving(&id), "a cancelled login saves nothing");
        let done = PendingState::Done {
            outcome: PendingOutcome::Consented { tenant: "t".into() },
        };
        assert!(table.finish(&id, done).is_none(), "cancel wins");
        assert_eq!(table.active(), 0);
    }

    #[test]
    fn finished_entries_expire_but_active_ones_stay() {
        let table = PendingTable::default();
        let (waiting, _rx) = table.begin(PendingKind::Login);
        let (cancelled, _rx2) = table.begin(PendingKind::Login);
        table.cancel(&cancelled);
        assert!(table.get(&cancelled).is_some(), "kept for late waiters");
        table.lock().expire_after(Duration::ZERO);
        assert!(table.get(&cancelled).is_none());
        assert_eq!(table.get(&waiting).unwrap().state, PendingState::Waiting);
    }

    #[test]
    fn cancel_waiting_leaves_a_save_alone() {
        let table = PendingTable::default();
        let (waiting, mut rx) = table.begin(PendingKind::Login);
        let (saving, _rx2) = table.begin(PendingKind::AddAccount);
        assert!(table.start_saving(&saving));
        table.cancel_waiting();
        assert_eq!(table.get(&waiting).unwrap().state, PendingState::Cancelled);
        assert!(rx.try_recv().is_ok());
        assert_eq!(table.get(&saving).unwrap().state, PendingState::Saving);
    }

    #[test]
    fn a_save_in_progress_cannot_be_cancelled() {
        let table = PendingTable::default();
        let (id, _rx) = table.begin(PendingKind::AddAccount);
        assert!(table.start_saving(&id));
        assert_eq!(table.cancel(&id).unwrap().state, PendingState::Saving);
        let failed = PendingState::Failed {
            error: OpError::internal("disk"),
        };
        assert!(table.finish(&id, failed).is_some());
    }

    #[tokio::test]
    async fn wait_returns_on_finish_or_after_the_timeout() {
        let table = std::sync::Arc::new(PendingTable::default());
        let (id, _rx) = table.begin(PendingKind::AdminConsent);
        let still = table.wait(&id, Duration::from_millis(50)).await.unwrap();
        assert_eq!(still.state, PendingState::Waiting);
        let finisher = table.clone();
        let finish_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let done = PendingState::Done {
                outcome: PendingOutcome::Consented { tenant: "t".into() },
            };
            finisher.finish(&finish_id, done);
        });
        let done = table.wait(&id, Duration::from_secs(5)).await.unwrap();
        assert!(done.state.is_finished());
        assert!(
            table
                .wait(&PendingId("nope".into()), Duration::ZERO)
                .await
                .is_none()
        );
    }
}
