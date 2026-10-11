//! Every account's sync status, held in memory, updated by its engine and published to the
//! clients that asked for `sync.progress`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ops::AccountSyncStatus;

/// Told of every change; returns false once its client is gone, and is then dropped.
pub type Subscriber = Box<dyn Fn(&AccountSyncStatus) -> bool + Send>;

#[derive(Default)]
struct Inner {
    accounts: BTreeMap<String, AccountSyncStatus>,
    subscribers: Vec<Subscriber>,
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
}
