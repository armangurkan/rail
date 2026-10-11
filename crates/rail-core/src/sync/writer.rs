//! The writer: one thread that owns the only connection writing sync data. Engines and the
//! search operation send `Batch` values over a bounded channel of `WRITER_QUEUE` batches, so a
//! fast provider waits for the disk; each batch commits in one transaction.

use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::batch::{Batch, BatchOutcome};
use crate::auth::unix_now;
use crate::error::ErrorClass;
use crate::ops::{ErrorCode, OpError};
use crate::store::Store;

/// Batches that may wait for the writer.
pub const WRITER_QUEUE: usize = 4;

type Job = (Batch, oneshot::Sender<Result<BatchOutcome, OpError>>);

/// Where batches go. Cheap to clone; the writer ends once every handle is dropped.
#[derive(Clone)]
pub struct WriterHandle {
    jobs: mpsc::Sender<Job>,
}

/// Waits for the writer to end.
pub struct WriterTask {
    done: oneshot::Receiver<()>,
}

/// The writer thread is gone (ended or panicked): retrying cannot help, so it is permanent.
fn stopped() -> OpError {
    OpError::new(
        ErrorCode::Store,
        ErrorClass::Permanent,
        "the store writer has stopped",
    )
}

impl WriterHandle {
    /// True once the writer thread is gone (it panicked, or it ended): every write fails.
    pub fn is_closed(&self) -> bool {
        self.jobs.is_closed()
    }

    /// Queue `batch` (waiting while the queue is full) and wait for its commit.
    pub async fn write(&self, batch: Batch) -> Result<BatchOutcome, OpError> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .send((batch, reply))
            .await
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }
}

impl WriterTask {
    /// After every handle is dropped, wait at most `grace` for the batch in hand to commit.
    /// False when the grace ran out or the writer thread died (panicked) instead of ending.
    pub async fn finish(self, grace: Duration) -> bool {
        matches!(tokio::time::timeout(grace, self.done).await, Ok(Ok(())))
    }
}

/// Open the store (migrating it) and start the writer thread.
pub fn spawn_writer(db: &Path) -> Result<(WriterHandle, WriterTask), OpError> {
    spawn(db, None)
}

/// `spawn_writer` whose every batch waits for a permit from `permits` first.
#[cfg(test)]
pub(crate) fn spawn_gated(
    db: &Path,
    permits: std::sync::mpsc::Receiver<()>,
) -> Result<(WriterHandle, WriterTask), OpError> {
    spawn(db, Some(permits))
}

/// `spawn_writer`; when `permits` is given (tests), each batch waits for a permit first.
fn spawn(
    db: &Path,
    permits: Option<std::sync::mpsc::Receiver<()>>,
) -> Result<(WriterHandle, WriterTask), OpError> {
    let mut store = Store::open(db)?;
    let (jobs, mut queue) = mpsc::channel::<Job>(WRITER_QUEUE);
    let (finished, done) = oneshot::channel();
    std::thread::Builder::new()
        .name("rail-writer".into())
        .spawn(move || {
            while let Some((batch, reply)) = queue.blocking_recv() {
                if let Some(permits) = &permits
                    && permits.recv().is_err()
                {
                    break;
                }
                #[cfg(test)]
                if batch.panic_writer {
                    panic!("test: the writer thread panics");
                }
                let now = batch.at.unwrap_or_else(unix_now);
                let outcome = store.apply(&batch, now).map_err(OpError::from);
                if let Err(error) = &outcome {
                    tracing::warn!(account = batch.account_id, error = %error.message, "a sync batch was rolled back");
                }
                let _ = reply.send(outcome);
            }
            let _ = finished.send(());
        })
        .map_err(|e| OpError::io(ErrorCode::Store, "starting the store writer", &e))?;
    Ok((WriterHandle { jobs }, WriterTask { done }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::mock::envelope;
    use crate::sync::batch::Batch;

    fn account(db: &Path) {
        Store::open(db)
            .unwrap()
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('w', 'gmail', 'w@example.com')",
                [],
            )
            .unwrap();
    }

    fn page(start: usize, n: usize) -> Batch {
        Batch {
            upserts: (start..start + n)
                .map(|i| envelope(&format!("m{i}"), &format!("t{i}"), 1_000 + i as i64))
                .collect(),
            ..Batch::new(1)
        }
    }

    fn count(db: &Path) -> i64 {
        Store::open_reader(db)
            .unwrap()
            .conn()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    }

    const WAIT: Duration = Duration::from_secs(5);

    /// Spec §8: readers on their own connections see a batch whole or not at all.
    #[tokio::test]
    async fn readers_never_see_half_a_batch() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        const ROUNDS: usize = 5;
        const PAGE: usize = 400;
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (writer, _task) = spawn_writer(&db).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
        let reader = std::thread::spawn({
            let (db, stop, seen) = (db.clone(), stop.clone(), seen.clone());
            move || {
                let reader = Store::open_reader(&db).unwrap();
                while !stop.load(Ordering::SeqCst) {
                    let n: i64 = reader
                        .conn()
                        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                        .unwrap();
                    seen.lock().unwrap().insert(n);
                }
            }
        });
        for round in 0..ROUNDS {
            writer.write(page(round * PAGE, PAGE)).await.unwrap();
            // The count stays put until the next write: wait for the reader to see it.
            let committed = ((round + 1) * PAGE) as i64;
            tokio::time::timeout(WAIT, async {
                while !seen.lock().unwrap().contains(&committed) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        let seen = seen.lock().unwrap().clone();
        let total = (ROUNDS * PAGE) as i64;
        assert!(
            seen.iter().any(|n| 0 < *n && *n < total),
            "the reader read between batches: {seen:?}"
        );
        assert!(
            seen.iter().all(|n| *n % PAGE as i64 == 0),
            "a reader saw part of a batch: {seen:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_batch_does_not_stop_the_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (writer, task) = spawn_writer(&db).unwrap();
        let poisoned = Batch {
            poison: true,
            ..page(0, 3)
        };
        assert!(writer.write(poisoned).await.is_err());
        assert_eq!(count(&db), 0);
        let outcome = writer.write(page(10, 2)).await.unwrap();
        assert_eq!(outcome.upserted, 2);
        assert_eq!(count(&db), 2);
        drop(writer);
        assert!(task.finish(WAIT).await);
    }

    #[tokio::test]
    async fn a_dead_writer_is_permanent_and_finish_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (writer, task) = spawn_writer(&db).unwrap();
        let fatal = Batch {
            panic_writer: true,
            ..page(0, 1)
        };
        let error = writer.write(fatal).await.unwrap_err();
        assert_eq!(error.class, ErrorClass::Permanent);
        let after = writer.write(page(1, 1)).await.unwrap_err();
        assert_eq!(
            (after.code, after.class),
            (ErrorCode::Store, ErrorClass::Permanent)
        );
        drop(writer);
        assert!(
            !task.finish(WAIT).await,
            "a panicked writer must not look healthy"
        );
        assert_eq!(count(&db), 0);
    }

    #[tokio::test]
    async fn the_bounded_queue_applies_backpressure() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (permits, held) = std::sync::mpsc::channel::<()>();
        let (writer, _task) = spawn(&db, Some(held)).unwrap();
        // The writer takes one batch and waits for a permit; WRITER_QUEUE more fit the queue.
        let mut pending = Vec::new();
        for i in 0..=WRITER_QUEUE {
            let writer = writer.clone();
            pending.push(tokio::spawn(
                async move { writer.write(page(i * 10, 1)).await },
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (probe, _) = tokio::sync::oneshot::channel();
        assert!(
            writer.jobs.try_send((page(999, 1), probe)).is_err(),
            "the queue is full while the writer is held"
        );
        for _ in 0..=WRITER_QUEUE {
            permits.send(()).unwrap();
        }
        for task in pending {
            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn finish_lets_the_batch_in_hand_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (permits, held) = std::sync::mpsc::channel::<()>();
        let (writer, task) = spawn(&db, Some(held)).unwrap();
        let sent = tokio::spawn({
            let writer = writer.clone();
            async move { writer.write(page(0, 50)).await }
        });
        drop(writer);
        // The writer holds the batch (waiting for a permit) while `finish` waits.
        let finishing = tokio::spawn(task.finish(WAIT));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !finishing.is_finished(),
            "finish waits for the batch in hand"
        );
        permits.send(()).unwrap();
        tokio::time::timeout(WAIT, sent)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(WAIT, finishing)
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(count(&db), 50);
    }

    #[tokio::test]
    async fn finish_is_false_when_the_grace_runs_out() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (permits, held) = std::sync::mpsc::channel::<()>();
        let (writer, task) = spawn(&db, Some(held)).unwrap();
        let sent = tokio::spawn({
            let writer = writer.clone();
            async move { writer.write(page(0, 5)).await }
        });
        drop(writer);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.finish(Duration::from_millis(50)).await);
        permits.send(()).unwrap();
        tokio::time::timeout(WAIT, sent)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(count(&db), 5);
    }
}
