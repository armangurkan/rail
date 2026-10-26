//! The snooze calls. `snooze.set` answers as an `action.apply` would (its rows are the archive's,
//! so `z` takes a snooze back like any action); `snooze.clear` puts threads back now; the
//! `snooze.woke` notification says which threads a snooze put back.

use rail_core::daemon::Connector;
use rail_core::daemon::wire::Notification;
use rail_core::ops::op::{SnoozeClear, SnoozeSet};
use rail_core::ops::{
    ActionApplyResponse, OpError, SnoozeClearRequest, SnoozeSetRequest, SnoozeWoke,
};

use super::{ApplyCall, DaemonLink, Link};
use crate::tui::app::{Effect, TaskSink};
use crate::tui::data::{DaemonEvent, MailEvent, job, send};

/// `snooze.set` for `call`'s threads until `until`, answered as `action.apply` answers.
pub(super) async fn set(
    connector: &Connector,
    call: &ApplyCall,
    until: i64,
) -> Result<ActionApplyResponse, OpError> {
    let request = SnoozeSetRequest {
        account: call.account.clone(),
        threads: call.threads.clone(),
        until,
        grace: call.grace,
        client_token: call.client_token.clone(),
    };
    let answer = connector.call::<SnoozeSet>(&request).await?;
    Ok(ActionApplyResponse {
        rows: answer.rows,
        applied: answer.snoozed,
    })
}

impl DaemonLink {
    /// Put one account's snoozed `threads` back in the inbox now.
    pub fn unsnooze(&self, account: String, threads: Vec<i64>) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = SnoozeClearRequest { account, threads };
                let result = connector.call::<SnoozeClear>(&request).await;
                send(&sink, MailEvent::Unsnoozed { result });
            }),
            #[cfg(test)]
            Link::Fake(fake) => {
                job(move |sink| async move { fake.unsnooze(&sink, account, threads) })
            }
        }
    }
}

/// `snooze.woke`: the threads a snooze put back.
pub(super) fn woke(note: Notification, sink: &TaskSink) {
    if let Ok(woke) = serde_json::from_value::<SnoozeWoke>(note.params) {
        send(sink, MailEvent::Daemon(DaemonEvent::Woke(woke)));
    } else {
        // The daemon's own JSON; its text is not repeated, the next re-read shows the threads.
        tracing::warn!("a snooze.woke notification did not parse");
    }
}

#[cfg(test)]
impl super::FakeDaemon {
    /// `snooze.set`: recorded, then answered as `action.apply` is (a fresh row when unset).
    pub(super) fn snooze(&self, sink: &TaskSink, call: ApplyCall, until: i64) {
        use std::sync::atomic::Ordering;
        self.record(super::FakeCall::SnoozeSet {
            account: call.account.clone(),
            threads: call.threads.clone(),
            until,
            grace: call.grace,
        });
        let answer = self
            .snooze_answer
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                let row = self.next_row.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(rail_core::ops::SnoozeSetResponse {
                    rows: vec![row],
                    snoozed: call.threads.len(),
                })
            });
        let result = if self.down() {
            Err(super::closed())
        } else {
            answer.map(|a| ActionApplyResponse {
                rows: a.rows,
                applied: a.snoozed,
            })
        };
        send(
            sink,
            MailEvent::Applied {
                call: Box::new(call),
                result,
            },
        );
    }

    /// `snooze.clear`: recorded, and every thread returned.
    pub(super) fn unsnooze(&self, sink: &TaskSink, account: String, threads: Vec<i64>) {
        let returned = threads.len();
        self.record(super::FakeCall::SnoozeClear { account, threads });
        let result = if self.down() {
            Err(super::closed())
        } else {
            Ok(rail_core::ops::SnoozeClearResponse { returned })
        };
        send(sink, MailEvent::Unsnoozed { result });
    }
}
