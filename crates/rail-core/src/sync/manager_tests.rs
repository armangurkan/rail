//! The sync manager without a socket: a writer that dies is started again with every engine,
//! a bounded number of times, and then sync stops and says so.

use std::sync::Arc;
use std::time::Duration;

use super::MAX_WRITER_RESTARTS;
use crate::daemon::DaemonContext;
use crate::ops::{AccountSyncStatus, DoctorReport, ErrorCode, SyncPhase};
use crate::paths::Paths;
use crate::provider::mock::{MockProvider, envelope};
use crate::sync::batch::Batch;
use crate::test_support::{daemon_context, mock_factory};

const WORK: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"5s\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

fn context(tmp: &tempfile::TempDir, mock: &MockProvider) -> Arc<DaemonContext> {
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _) = daemon_context(&paths);
    Arc::new(ctx.with_provider_factory(mock_factory(mock)))
}

async fn until(
    ctx: &DaemonContext,
    done: impl Fn(&AccountSyncStatus) -> bool,
) -> AccountSyncStatus {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = ctx.sync.board().get("me@example.com")
                && done(&status)
            {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the status was reached in time")
}

/// Make the writer thread panic, as a bug in a batch would.
async fn kill_the_writer(ctx: &DaemonContext) {
    let writer = ctx.sync.writer(ctx).await.unwrap();
    let batch = Batch {
        panic_writer: true,
        ..Batch::new(1)
    };
    let err = writer.write(batch).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Store);
    assert!(writer.is_closed());
}

fn report() -> DoctorReport {
    DoctorReport {
        config_path: "/c".into(),
        config_accounts: Some(1),
        config_error: None,
        db_path: "/d".into(),
        db_size_bytes: None,
        schema_version: Some(2),
        store_error: None,
        secrets_backend: "file".into(),
        secrets_file: None,
        secrets_error: None,
        secrets_checked: true,
        accounts: Vec::new(),
    }
}

#[tokio::test]
async fn a_dead_writer_is_restarted_with_every_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", crate::auth::unix_now() * 1000));
    let ctx = context(&tmp, &mock);
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1).await;
    kill_the_writer(&ctx).await;
    ctx.sync.check_writer(&ctx).await;
    let back = until(&ctx, |s| s.passes_completed >= 1 && !s.last_pass_failed).await;
    assert_eq!(back.messages, 1);
    let writer = ctx.sync.writer(&ctx).await.unwrap();
    assert!(!writer.is_closed(), "a new writer took over");
    writer.write(Batch::new(1)).await.unwrap();
    assert!(ctx.sync.writer_failure().is_none());
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_writer_that_keeps_dying_stops_sync_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(&tmp, &MockProvider::new());
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1).await;
    for _ in 0..MAX_WRITER_RESTARTS {
        kill_the_writer(&ctx).await;
        ctx.sync.check_writer(&ctx).await;
        until(&ctx, |s| s.passes_completed >= 1).await;
    }
    kill_the_writer(&ctx).await;
    ctx.sync.check_writer(&ctx).await;
    let failure = ctx.sync.writer_failure().expect("sync gave up");
    assert!(failure.contains("sync is paused"), "{failure}");
    let status = ctx.sync.board().get("me@example.com").unwrap();
    assert_eq!(status.phase, SyncPhase::Stopped);
    assert_eq!(status.last_error.as_ref().unwrap().code, "store");
    let refused = ctx.sync.writer(&ctx).await.err().expect("no writer");
    assert_eq!(refused.code, ErrorCode::Store);
    let err = ctx.sync.wake(Some("work")).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Store, "{err}");
    let mut doctor = report();
    ctx.sync.add_to_report(&mut doctor);
    assert!(!doctor.is_healthy());
    ctx.sync.reconcile(&ctx).await;
    assert!(
        ctx.sync.search_targets(None).await.is_empty(),
        "no engine starts again"
    );
}

/// PF18: `sync.now` on an account whose engine failed to start answers with that error.
#[tokio::test]
async fn waking_an_account_that_failed_to_start_gives_its_start_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let ctx = ctx.with_provider_factory(Arc::new(|_| {
        Err(crate::ops::OpError::invalid_input("1Password is locked"))
    }));
    ctx.sync.reconcile(&ctx).await;
    let err = ctx.sync.wake(Some("work")).await.unwrap_err();
    assert_eq!(err.message, "1Password is locked");
    let status = ctx.sync.status(Some("work")).await.unwrap().remove(0);
    assert_eq!(status.last_error.unwrap().code, "start");
}

/// PF12: an invalid config is read once per change, and leaves the running engine alone.
#[tokio::test]
async fn an_invalid_config_leaves_the_engines_and_is_not_read_again_until_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(&tmp, &MockProvider::new());
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1).await;
    std::fs::write(ctx.paths.config_file(), "[[accounts]\nbroken").unwrap();
    ctx.sync.refresh_if_changed(&ctx).await;
    assert_eq!(ctx.sync.search_targets(None).await.len(), 1);
    assert!(ctx.sync.has_accounts());
    // The broken file's time is remembered, so the next check does not read it again.
    let mtime = std::fs::metadata(ctx.paths.config_file())
        .and_then(|m| m.modified())
        .ok();
    assert!(mtime.is_some());
    assert_eq!(*ctx.sync.config_mtime.lock().unwrap(), mtime);
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}
