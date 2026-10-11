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

fn stopped() -> OpError {
    OpError::new(
        ErrorCode::Store,
        ErrorClass::Transient,
        "the store writer has stopped",
    )
}

impl WriterHandle {
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
    pub async fn finish(self, grace: Duration) -> bool {
        tokio::time::timeout(grace, self.done).await.is_ok()
    }
}

/// Open the store (migrating it) and start the writer thread.
pub fn spawn_writer(db: &Path) -> Result<(WriterHandle, WriterTask), OpError> {
    spawn(db, None)
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

    /// Spec §8: readers on their own connections see a batch whole or not at all.
    #[tokio::test]
    async fn readers_never_see_half_a_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("rail.db");
        account(&db);
        let (writer, _task) = spawn_writer(&db).unwrap();
        let reader_db = db.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = std::thread::spawn(move || {
            let reader = Store::open_reader(&reader_db).unwrap();
            let mut seen = std::collections::BTreeSet::new();
            while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                let n: i64 = reader
                    .conn()
                    .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                    .unwrap();
                seen.insert(n);
            }
            seen
        });
        for round in 0..5 {
            writer.write(page(round * 400, 400)).await.unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let seen = reader.join().unwrap();
        assert!(
            seen.iter().all(|n| n % 400 == 0),
            "a reader saw part of a batch: {seen:?}"
        );
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
        let (writer, task) = spawn_writer(&db).unwrap();
        let sent = tokio::spawn({
            let writer = writer.clone();
            async move { writer.write(page(0, 50)).await }
        });
        drop(writer);
        sent.await.unwrap().unwrap();
        assert!(task.finish(std::time::Duration::from_secs(5)).await);
        let n: i64 = Store::open_reader(&db)
            .unwrap()
            .conn()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 50);
    }
}
