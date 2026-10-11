//! The periodic checks: the writer, the engines and `config.toml`, every `CONFIG_CHECK`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::parts::{Checked, engine_ended, finish_writer, restarts_after, stop_engine, stopped_by};
use super::{MAX_WRITER_RESTARTS, SyncManager, lock};
use crate::daemon::DaemonContext;
use crate::ops::{ErrorCode, OpError};

/// How long a dead writer's task is given to report how it ended.
const DEAD_WRITER_GRACE: Duration = Duration::from_secs(1);

impl SyncManager {
    /// Every `CONFIG_CHECK`: look at the writer and the engines, then reconcile when
    /// `config.toml` changed or a failed account is due another try. On a task of its own; a
    /// tick that finds the previous check still running skips.
    pub fn tick(ctx: &Arc<DaemonContext>) {
        if ctx.sync.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _done = Checked(&ctx.sync.checking);
            ctx.sync.check_writer(&ctx).await;
            ctx.sync.check_engines().await;
            ctx.sync.refresh_if_changed(&ctx).await;
        });
    }

    /// Reconcile when `config.toml` changed on disk since the last look, or a failed account
    /// is due another try.
    pub async fn refresh_if_changed(&self, ctx: &DaemonContext) {
        let path = ctx.paths.config_file();
        let now = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let seen = *lock(&self.config_mtime);
        let due = self.writer_failure().is_none() && self.engines.lock().await.retry_due();
        if now != seen || due {
            self.reconcile(ctx).await;
        }
    }

    /// Notice engines whose task ended on its own (a panic): their status says so, and they
    /// are started again like a failed start, after the same backoff.
    pub async fn check_engines(&self) {
        let mut engines = self.engines.lock().await;
        let ended: Vec<String> = engines
            .running
            .iter()
            .filter(|(_, running)| running.task.is_finished())
            .map(|(address, _)| address.clone())
            .collect();
        for address in ended {
            let Some(running) = engines.running.remove(&address) else {
                continue;
            };
            if let Err(error) = running.task.await
                && error.is_panic()
            {
                tracing::error!(account = %running.name, "a sync engine panicked");
            } else {
                tracing::error!(account = %running.name, "a sync engine ended on its own");
            }
            let error = engine_ended();
            self.board
                .update(&address, |s| stopped_by(s, "engine", &error));
            engines.record_failure(&address, error);
        }
    }

    /// Notice a writer that died (it panicked, or its thread ended): every engine's writes
    /// would fail from now on. Restart it with every engine, at most `MAX_WRITER_RESTARTS`
    /// times (a writer that stayed up `WRITER_HEALTHY` starts the count again); after that
    /// stop syncing and say so in each account's status and in doctor.
    pub async fn check_writer(&self, ctx: &DaemonContext) {
        let mut engines = self.engines.lock().await;
        let dead = engines.writer.as_ref().is_some_and(|(w, _)| w.is_closed());
        if !dead || self.is_stopping() {
            return;
        }
        let healthy_for = engines.writer_started.map(|t| t.elapsed());
        engines.writer_restarts = restarts_after(engines.writer_restarts, healthy_for);
        for (_, running) in engines.running.drain() {
            stop_engine(running).await;
        }
        finish_writer(engines.writer.take(), DEAD_WRITER_GRACE).await;
        engines.writer_started = None;
        if engines.writer_restarts >= MAX_WRITER_RESTARTS {
            self.give_up_on_the_writer();
            return;
        }
        engines.writer_restarts += 1;
        tracing::warn!(
            attempt = engines.writer_restarts,
            "the store writer stopped; starting it and every engine again"
        );
        drop(engines);
        // An invalid config.toml must not leave sync stopped: the last good one is used.
        self.reconcile_with(ctx, true).await;
    }

    fn give_up_on_the_writer(&self) {
        let message = format!(
            "the store writer stopped {} times; sync is paused until the daemon restarts (`rail daemon stop`)",
            MAX_WRITER_RESTARTS + 1
        );
        tracing::error!("{message}");
        let error = OpError::new(
            ErrorCode::Store,
            crate::error::ErrorClass::Permanent,
            message.clone(),
        );
        for status in self.board.all() {
            self.board
                .update(&status.address, |s| stopped_by(s, "store", &error));
        }
        *lock(&self.writer_failure) = Some(message);
    }
}
