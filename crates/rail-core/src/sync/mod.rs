//! Sync: one writer that owns every sync write, one engine per account, the status board, and
//! the manager the daemon runs them through.

pub mod batch;
pub mod engine;
pub mod manager;
mod pass;
pub mod providers;
pub mod search;
pub mod status;
pub mod writer;

#[cfg(test)]
mod engine_tests;

pub use batch::{Batch, BatchOutcome, Origin, Prune, StateUpdate, Sweep};
pub use engine::{Clock, Engine, EngineParts, SyncError};
pub use manager::{
    CONFIG_CHECK, ENGINE_STOP, MAX_WRITER_RESTARTS, ProviderFactory, ProviderRequest, SETTLE_WAIT,
    SearchTarget, SyncManager, WRITER_HEALTHY,
};
pub use status::{StatusBoard, Subscriber};
pub use writer::{WRITER_QUEUE, WriterHandle, WriterTask, spawn_writer};

/// How long a message found only by server search is kept after it was last searched for.
pub const SEARCH_KEEP_DAYS: i64 = 14;
/// Bodies fetched at once.
pub const BODY_CONCURRENCY: usize = 4;
/// Bodies per writer batch.
pub const BODY_BATCH: usize = 16;
/// Bodies one pass fetches at most; the rest wait for the next pass.
pub const BODIES_PER_PASS: usize = 500;
/// Stored for a body Rail could not read, so it is not fetched again every pass.
pub const UNREADABLE_BODY: &str = "[Rail could not read this message's body.]";
/// Backfill pages one pass lists at most while a cursor exists (a grown window), so each pass
/// also catches up new mail within one poll interval.
pub const BACKFILL_PAGES_PER_PASS: usize = 20;
