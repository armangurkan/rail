//! What the wizard has in flight in the daemon, so that a cancel (Esc) or a quit ends it there,
//! and what a quit found: whether a login it cancelled had already reached its save.

use std::collections::HashSet;

use rail_core::error::ErrorClass;
use rail_core::onboarding::Failure;
use rail_core::ops::op::{Operation, PendingCancel};
use rail_core::ops::{
    BeginResponse, CancelRequest, OpError, PendingId, PendingOutcome, PendingState,
};

use super::{EffectContext, failure, lock};

/// The logins and consents this process has begun in the daemon and not seen finish.
#[derive(Default)]
pub(super) struct Outstanding {
    pub(super) ids: HashSet<PendingId>,
    /// Begin requests sent whose answer (and so whose id) has not arrived yet.
    begins: usize,
    /// A cancel found its login saving or saved: the daemon keeps that account.
    kept: bool,
}

/// What `EffectContext::settle` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Nothing in flight was saved: everything left was cancelled before its save.
    Clean,
    /// A login cancelled on the way out (or by Esc) had already reached its save, which the
    /// daemon finishes: that account exists although the wizard did not report it.
    SaveKept,
}

impl EffectContext {
    /// Cancel every login or consent still in flight, waiting first for begins the daemon has
    /// not answered yet. Called once the event loop ends, so quitting mid-login leaves nothing
    /// waiting in the daemon. Bounded: a daemon that does not answer cannot hold the exit.
    pub async fn settle(&self) -> Settled {
        if tokio::time::timeout(self.cancel_timeout, self.cancel_outstanding())
            .await
            .is_err()
        {
            tracing::warn!(
                "the daemon did not confirm the cancel in time; a login may still be pending"
            );
        }
        if lock(&self.outstanding).kept {
            Settled::SaveKept
        } else {
            Settled::Clean
        }
    }

    async fn cancel_outstanding(&self) {
        self.begins_answered().await;
        let ids: Vec<PendingId> = lock(&self.outstanding).ids.iter().cloned().collect();
        for id in ids {
            cancel_in_daemon(self, id).await;
        }
    }

    async fn begins_answered(&self) {
        loop {
            let answered = self.answered.notified();
            tokio::pin!(answered);
            answered.as_mut().enable();
            if lock(&self.outstanding).begins == 0 {
                return;
            }
            answered.await;
        }
    }
}

/// True when a cancel's answer says the account is (being) saved anyway.
fn saved_anyway(state: &PendingState) -> bool {
    matches!(
        state,
        PendingState::Saving
            | PendingState::Done {
                outcome: PendingOutcome::Added { .. } | PendingOutcome::LoggedIn { .. },
            }
    )
}

/// Ask the daemon to cancel `id`, then forget it. A daemon that cannot be reached has nothing
/// left to cancel for this process; an id the daemon no longer knows has already finished.
async fn cancel_in_daemon(ctx: &EffectContext, id: PendingId) {
    let request = CancelRequest { id: id.clone() };
    match ctx.daemon.call::<PendingCancel>(&request).await {
        Ok(answer) if saved_anyway(&answer.state) => {
            tracing::info!(id = %id, "cancelled too late: the daemon keeps the saved account");
            lock(&ctx.outstanding).kept = true;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(id = %id, error = %e.message, "the cancel was refused"),
    }
    lock(&ctx.outstanding).ids.remove(&id);
}

/// Counts a begin request as unanswered while it lives.
struct Answering(EffectContext);

impl Answering {
    fn new(ctx: &EffectContext) -> Self {
        lock(&ctx.outstanding).begins += 1;
        Self(ctx.clone())
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        lock(&self.0.outstanding).begins -= 1;
        self.0.answered.notify_waiters();
    }
}

/// Marks a begun operation as in flight. Dropped before `disarm` (the job was aborted by a
/// cancel or a quit), it asks the daemon to cancel the operation; `settle` cancels it too if
/// that cannot finish before the process exits.
pub(super) struct InFlight {
    ctx: EffectContext,
    id: Option<PendingId>,
}

impl InFlight {
    fn new(ctx: &EffectContext, id: &PendingId) -> Self {
        lock(&ctx.outstanding).ids.insert(id.clone());
        Self {
            ctx: ctx.clone(),
            id: Some(id.clone()),
        }
    }

    /// The operation finished: nothing to cancel.
    pub(super) fn disarm(mut self) {
        if let Some(id) = self.id.take() {
            lock(&self.ctx.outstanding).ids.remove(&id);
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let ctx = self.ctx.clone();
            runtime.spawn(async move { cancel_in_daemon(&ctx, id).await });
        }
    }
}

/// Send a begin request in a task of its own, so a job aborted while the daemon answers still
/// learns the id: the task's unread output, the `InFlight`, is then dropped and cancels it.
pub(super) async fn begin<O>(
    ctx: &EffectContext,
    request: O::Request,
) -> Result<(BeginResponse, InFlight), Failure>
where
    O: Operation<Response = BeginResponse>,
{
    let answering = Answering::new(ctx);
    let ctx = ctx.clone();
    let task = tokio::spawn(async move {
        let _answering = answering;
        let begun = ctx.daemon.call::<O>(&request).await?;
        let in_flight = InFlight::new(&ctx, &begun.id);
        Ok::<_, OpError>((begun, in_flight))
    });
    match task.await {
        Ok(result) => result.map_err(|e| failure(&e)),
        Err(e) => Err(Failure::new(
            format!("internal error: {e}"),
            ErrorClass::Permanent,
        )),
    }
}
