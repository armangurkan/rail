//! Sync: one writer that owns every sync write, one engine per account, the status board, and
//! the manager the daemon runs them through.

pub mod batch;
pub mod writer;

pub use batch::{Batch, BatchOutcome, Origin, Prune, StateUpdate, Sweep};
pub use writer::{WRITER_QUEUE, WriterHandle, WriterTask, spawn_writer};

/// How long a message found only by server search is kept after it was last searched for.
pub const SEARCH_KEEP_DAYS: i64 = 14;
