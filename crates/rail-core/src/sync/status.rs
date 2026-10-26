//! Every account's sync status, held in memory, updated by its engine and published to the
//! clients that asked for `sync.progress`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ops::{AccountSyncStatus, ActionFailed, SnoozeWoke};

/// Told of every change; returns false once its client is gone, and is then dropped.
pub type Subscriber = Box<dyn Fn(&AccountSyncStatus) -> bool + Send>;

/// Told of every action Gmail refused for good; returns false once its client is gone.
pub type FailureSubscriber = Box<dyn Fn(&ActionFailed) -> bool + Send>;

/// Told of every snooze that put threads back; returns false once its client is gone.
pub type WokeSubscriber = Box<dyn Fn(&SnoozeWoke) -> bool + Send>;

#[derive(Default)]
struct Inner {
    accounts: BTreeMap<String, AccountSyncStatus>,
    subscribers: Vec<Subscriber>,
    failure_subscribers: Vec<FailureSubscriber>,
    woke_subscribers: Vec<WokeSubscriber>,
}

#[derive(Clone, Default)]
pub struct StatusBoard {
    inner: Arc<Mutex<Inner>>,
}

impl StatusBoard {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn insert(&self, status: AccountSyncStatus) {
        self.lock()
            .accounts
            .insert(status.address.to_ascii_lowercase(), status);
    }

    pub fn remove(&self, address: &str) {
        self.lock().accounts.remove(&address.to_ascii_lowercase());
    }

    /// Change one account's status and tell every subscriber.
    pub fn update(&self, address: &str, change: impl FnOnce(&mut AccountSyncStatus)) {
        let mut inner = self.lock();
        let Some(status) = inner.accounts.get_mut(&address.to_ascii_lowercase()) else {
            return;
        };
        change(status);
        let snapshot = status.clone();
        inner.subscribers.retain(|tell| tell(&snapshot));
    }

    pub fn get(&self, address: &str) -> Option<AccountSyncStatus> {
        self.lock()
            .accounts
            .get(&address.to_ascii_lowercase())
            .cloned()
    }

    pub fn all(&self) -> Vec<AccountSyncStatus> {
        self.lock().accounts.values().cloned().collect()
    }

    pub fn subscribe(&self, subscriber: Subscriber) {
        self.lock().subscribers.push(subscriber);
    }

    pub fn subscribe_failures(&self, subscriber: FailureSubscriber) {
        self.lock().failure_subscribers.push(subscriber);
    }

    /// Tell every failure subscriber; ones whose client is gone are dropped.
    pub fn report_failure(&self, failure: &ActionFailed) {
        self.lock().failure_subscribers.retain(|tell| tell(failure));
    }

    pub fn subscribe_wakes(&self, subscriber: WokeSubscriber) {
        self.lock().woke_subscribers.push(subscriber);
    }

    /// Tell every wake subscriber; ones whose client is gone are dropped.
    pub fn report_woke(&self, woke: &SnoozeWoke) {
        self.lock().woke_subscribers.retain(|tell| tell(woke));
    }
}
