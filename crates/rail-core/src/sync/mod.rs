//! Sync: one writer that owns every sync write, one engine per account, the status board, and
//! the manager the daemon runs them through.

mod backfill;
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

pub use batch::{Batch, BatchOutcome, Origin, Prune, ResumeToken, StateUpdate, Sweep};
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
/// Bodies per writer batch at most.
pub const BODY_BATCH: usize = 16;
/// Text and HTML bytes after which a body batch goes to the writer, however few bodies it has.
pub const BODY_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Bodies one pass fetches at most; the rest wait for the next pass.
pub const BODIES_PER_PASS: usize = 500;
/// Stored for a body Rail could not read, so it is not fetched again every pass.
pub const UNREADABLE_BODY: &str = "[Rail could not read this message's body.]";
/// Backfill pages one pass lists at most while a cursor exists (a grown window), so each pass
/// also catches up new mail within one poll interval.
/// Only pages that made progress (stored a message) count; the others go on by page token.
pub const BACKFILL_PAGES_PER_PASS: usize = 20;
/// Listing pages one pass asks for at most, so a pathological listing cannot hold a pass: in
/// all for a capped backfill (a grown window), and pages without progress for the first full
/// backfill (its pages with progress each store new mail, so the mailbox bounds them).
pub const BACKFILL_MAX_PAGES_PER_PASS: usize = 200;
/// How far above the oldest message stored a backfill lists again when it cannot go on with
/// Gmail's page token: three days. Gmail lists only roughly newest first (the live test saw
/// a message listed up to ~9.5 hours out of order), so mail newer than the marker can still be
/// unlisted; three days is about 7 times the spread seen, and costs only list calls, since ids
/// already stored are not fetched again.
pub const BACKFILL_OVERLAP_MS: i64 = 3 * 86_400_000;
/// A stored page token older than this (seconds) is not used: the backfill lists from its
/// marker with the overlap instead. A resume normally comes one poll interval later; an older
/// token means the daemon was off or the machine asleep, Gmail does not document how long a
/// token lives, and the mailbox may have changed under the listing it belongs to.
pub const BACKFILL_TOKEN_MAX_AGE_SECS: i64 = 3_600;
