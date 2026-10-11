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
    // The panicking thread drops the batch's reply (so `write` fails) before unwinding drops
    // the queue, so the handle may report closed a moment later.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !writer.is_closed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the dead writer's queue closed");
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
    let err = ctx.sync.wake(&ctx, Some("work")).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Store, "{err}");
    let mut doctor = report();
    ctx.sync.add_to_report(&mut doctor);
    assert!(!doctor.is_healthy());
    ctx.sync.reconcile(&ctx).await;
    assert!(
        ctx.sync.search_plan(None).await.0.is_empty(),
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
    let ctx = Arc::new(ctx.with_provider_factory(Arc::new(|_| {
        Err(crate::ops::OpError::invalid_input("1Password is locked"))
    })));
    ctx.sync.reconcile(&ctx).await;
    let err = ctx.sync.wake(&ctx, Some("work")).await.unwrap_err();
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
    assert_eq!(ctx.sync.search_plan(None).await.0.len(), 1);
    assert!(ctx.sync.has_accounts());
    // The broken file's time is remembered, so the next check does not read it again.
    let mtime = std::fs::metadata(ctx.paths.config_file())
        .and_then(|m| m.modified())
        .ok();
    assert!(mtime.is_some());
    assert_eq!(*ctx.sync.config_mtime.lock().unwrap(), mtime);
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

const WORK_AND_SLOW: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"5s\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n\n[[accounts]]\nname = \"slow\"\nkind = \"gmail\"\naddress = \"slow@example.com\"\nclient_id = \"cid\"\nclient_secret = \"op://Private/rail/secret\"\n";

/// Holds the factory for the account named "slow", as a locked 1Password holds `op read`.
#[derive(Default)]
struct Gate {
    entered: std::sync::atomic::AtomicBool,
    open: std::sync::Mutex<bool>,
    opened: std::sync::Condvar,
}

impl Gate {
    fn wait(&self) {
        self.entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let open = self.open.lock().unwrap();
        let _ = self
            .opened
            .wait_timeout_while(open, Duration::from_secs(10), |open| !*open)
            .unwrap();
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// Fix round 1: building a provider (an `op read` waiting on 1Password) holds no lock that
/// `sync.now`, `search` or shutdown need, and shutdown still stops the engines and the writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_provider_build_blocks_neither_requests_nor_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockProvider::new();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let gate = Arc::new(Gate::default());
    let (held, served) = (gate.clone(), mock.clone());
    let factory: super::ProviderFactory = Arc::new(move |request: super::ProviderRequest| {
        if request.account.name == "slow" {
            held.wait();
        }
        Ok(Arc::new(served.clone()) as Arc<dyn crate::provider::Provider>)
    });
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(factory));
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1).await;
    std::fs::write(paths.config_file(), WORK_AND_SLOW).unwrap();
    super::SyncManager::reconcile_in_background(ctx.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the slow build began");
    let quick = Duration::from_secs(2);
    let woken = tokio::time::timeout(quick, ctx.sync.wake(&ctx, Some("work"))).await;
    assert_eq!(woken.expect("sync.now did not wait").unwrap().len(), 1);
    let request = crate::ops::SearchRequest {
        query: "anything".into(),
        account: None,
        limit: 5,
    };
    let searched = tokio::time::timeout(quick, crate::sync::search::run(&ctx, request)).await;
    assert!(searched.expect("search did not wait").is_ok());
    tokio::time::timeout(quick * 5, ctx.sync.shutdown(Duration::from_secs(5)))
        .await
        .expect("shutdown did not wait for the slow build");
    let work = ctx.sync.board().get("me@example.com").unwrap();
    assert_eq!(work.phase, SyncPhase::Stopped, "the engine was stopped");
    assert!(
        ctx.sync.writer(&ctx).await.is_err(),
        "no writer after shutdown"
    );
    gate.release();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        ctx.sync.search_plan(None).await.0.is_empty(),
        "nothing starts after shutdown began"
    );
}

/// Fix round 1: a writer restart while `config.toml` is invalid uses the last good config.
#[tokio::test]
async fn a_writer_restart_with_an_invalid_config_uses_the_last_good_one() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(&tmp, &MockProvider::new());
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1).await;
    std::fs::write(ctx.paths.config_file(), "[[accounts]\nbroken").unwrap();
    ctx.sync.refresh_if_changed(&ctx).await;
    kill_the_writer(&ctx).await;
    ctx.sync.check_writer(&ctx).await;
    let back = until(&ctx, |s| s.passes_completed >= 1 && !s.last_pass_failed).await;
    assert_eq!(back.name, "work");
    assert_eq!(ctx.sync.search_plan(None).await.0.len(), 1);
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

/// A provider whose every call panics, as a bug in an engine step would.
struct Panics;

#[async_trait::async_trait]
impl crate::provider::Provider for Panics {
    async fn folders(
        &self,
    ) -> Result<Vec<crate::provider::RemoteFolder>, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn profile(&self) -> Result<crate::provider::Profile, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn backfill(
        &self,
        _: &crate::provider::Window,
        _: Option<&crate::provider::PageToken>,
    ) -> Result<crate::provider::EnvelopePage, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn changes(
        &self,
        _: &crate::provider::SyncCursor,
    ) -> Result<crate::provider::ChangeSet, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn body(&self, _: &str) -> Result<crate::provider::Body, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn search(
        &self,
        _: &str,
        _: usize,
    ) -> Result<Vec<crate::provider::RemoteId>, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
    async fn envelopes(
        &self,
        _: &[crate::provider::RemoteId],
    ) -> Result<crate::provider::EnvelopePage, crate::provider::ProviderError> {
        panic!("test: the engine panics")
    }
}

/// Fix round 1: an engine that ends on its own is noticed, shown as stopped with an error,
/// and started again (here by `sync.now`, which retries a failed account at once).
#[tokio::test]
async fn an_engine_that_panics_is_noticed_and_started_again() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let broken = Arc::new(AtomicBool::new(true));
    let (flag, mock) = (broken.clone(), MockProvider::new());
    let factory: super::ProviderFactory = Arc::new(move |_| {
        Ok(if flag.load(Ordering::SeqCst) {
            Arc::new(Panics) as Arc<dyn crate::provider::Provider>
        } else {
            Arc::new(mock.clone())
        })
    });
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(factory));
    ctx.sync.reconcile(&ctx).await;
    let stopped = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            ctx.sync.check_engines().await;
            let status = ctx.sync.board().get("me@example.com").unwrap();
            if status.phase == SyncPhase::Stopped {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the ended engine was noticed");
    assert_eq!(stopped.last_error.unwrap().code, "engine");
    assert!(ctx.sync.search_plan(None).await.0.is_empty());
    broken.store(false, Ordering::SeqCst);
    let targets = ctx.sync.wake(&ctx, Some("work")).await.unwrap();
    assert_eq!(targets.len(), 1, "started again");
    until(&ctx, |s| s.passes_completed >= 1 && !s.last_pass_failed).await;
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

#[test]
fn failed_starts_back_off_and_a_healthy_writer_resets_its_count() {
    use super::parts::{restarts_after, retry_delay};
    let minutes = |m: u64| Duration::from_secs(m * 60);
    assert_eq!(retry_delay(1), minutes(1));
    assert_eq!(retry_delay(2), minutes(5));
    assert_eq!(retry_delay(3), minutes(15));
    assert_eq!(retry_delay(9), minutes(15));
    assert_eq!(restarts_after(3, Some(minutes(61))), 0);
    assert_eq!(restarts_after(3, Some(minutes(5))), 3);
    assert_eq!(restarts_after(2, None), 2);
}

/// A factory: "slow" fails its first build, then each build waits on `gate`; others get `mock`.
fn slow_factory(gate: &Arc<Gate>, mock: &MockProvider, fail_first: bool) -> super::ProviderFactory {
    let (held, served) = (gate.clone(), mock.clone());
    let failed_once = std::sync::atomic::AtomicBool::new(!fail_first);
    Arc::new(move |request: super::ProviderRequest| {
        if request.account.name == "slow" {
            if !failed_once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::ops::OpError::invalid_input("1Password is locked"));
            }
            held.wait();
        }
        Ok(Arc::new(served.clone()) as Arc<dyn crate::provider::Provider>)
    })
}

fn slow_context(tmp: &tempfile::TempDir, factory: super::ProviderFactory) -> Arc<DaemonContext> {
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK_AND_SLOW).unwrap();
    let (ctx, _) = daemon_context(&paths);
    Arc::new(ctx.with_provider_factory(factory))
}

async fn entered(gate: &Gate) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the slow build began");
}

async fn reconciles_done(ctx: &DaemonContext) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while ctx.sync.reconciling.try_lock().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the reconcile ended");
}

/// Fix round 2: `sync.now` for every account wakes the healthy engines at once while a failed
/// account's retry waits on 1Password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waking_every_account_does_not_wait_for_a_failed_ones_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let ctx = slow_context(&tmp, slow_factory(&gate, &MockProvider::new(), true));
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1 && !s.pass_running).await;
    // The failed account's backoff has passed: the next reconcile retries it.
    if let Some(failed) = ctx
        .sync
        .engines
        .lock()
        .await
        .failed
        .get_mut("slow@example.com")
    {
        failed.retry_at = tokio::time::Instant::now();
    }
    super::SyncManager::reconcile_in_background(ctx.clone());
    entered(&gate).await;
    let woken = tokio::time::timeout(Duration::from_secs(2), ctx.sync.wake(&ctx, None))
        .await
        .expect("sync.now did not wait for the retry")
        .unwrap();
    assert_eq!(woken.len(), 1);
    assert_eq!(woken[0].name, "work");
    let target = woken[0].pass;
    until(&ctx, |s| s.passes_completed >= target).await;
    gate.release();
    reconciles_done(&ctx).await;
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

/// Fix round 2: an account removed from `config.toml` while its provider was being built
/// gets no engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removed_while_building_gets_no_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let ctx = slow_context(&tmp, slow_factory(&gate, &MockProvider::new(), false));
    super::SyncManager::reconcile_in_background(ctx.clone());
    entered(&gate).await;
    std::fs::write(ctx.paths.config_file(), WORK).unwrap();
    gate.release();
    reconciles_done(&ctx).await;
    let engines = ctx.sync.engines.lock().await;
    let running: Vec<&String> = engines.running.keys().collect();
    assert_eq!(
        running,
        ["me@example.com"],
        "only the account still configured"
    );
    drop(engines);
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}

/// Fix round 2: an invalid `config.toml` with a failed account due another try is warned
/// about once, not at every check.
#[tokio::test(start_paused = true)]
async fn an_invalid_config_is_warned_about_once_even_with_a_retry_due() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(Arc::new(|_| {
        Err(crate::ops::OpError::invalid_input("1Password is locked"))
    })));
    ctx.sync.reconcile(&ctx).await;
    std::fs::write(paths.config_file(), "[[accounts]\nbroken").unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    for _ in 0..10 {
        ctx.sync.refresh_if_changed(&ctx).await;
        tokio::time::advance(super::CONFIG_CHECK).await;
    }
    let failed = ctx
        .sync
        .failed_reconciles
        .load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(failed, 1, "one warning over ten checks");
}

/// A factory whose builds always fail, counting them.
fn failing_factory(builds: &Arc<std::sync::atomic::AtomicUsize>) -> super::ProviderFactory {
    let builds = builds.clone();
    Arc::new(move |_| {
        builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(crate::ops::OpError::invalid_input("1Password is locked"))
    })
}

fn builds_of(builds: &std::sync::atomic::AtomicUsize) -> usize {
    builds.load(std::sync::atomic::Ordering::SeqCst)
}

/// Review minor 4: reconciles that follow one another (the startup one and the first check,
/// say) leave a failed account to its backoff instead of each running `op read` for it.
#[tokio::test]
async fn back_to_back_reconciles_build_a_failed_account_once() {
    let tmp = tempfile::tempdir().unwrap();
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(failing_factory(&builds)));
    ctx.sync.reconcile(&ctx).await;
    ctx.sync.reconcile(&ctx).await;
    assert_eq!(builds_of(&builds), 1);
    ctx.sync.reconcile_now(&ctx, "work").await;
    assert_eq!(builds_of(&builds), 2, "asked for by name: tried at once");
}

/// Review minor 4: two unnamed `sync.now` in a row run at most one retry build, and none
/// before the failed account's backoff has passed.
#[tokio::test]
async fn unnamed_wakes_honour_a_failed_accounts_backoff() {
    let tmp = tempfile::tempdir().unwrap();
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(failing_factory(&builds)));
    ctx.sync.reconcile(&ctx).await;
    assert_eq!(builds_of(&builds), 1);
    for _ in 0..2 {
        ctx.sync.wake(&ctx, None).await.unwrap();
        // Time for a background reconcile, had one been started, to take its lock.
        tokio::time::sleep(Duration::from_millis(50)).await;
        reconciles_done(&ctx).await;
    }
    assert_eq!(builds_of(&builds), 1, "not before the backoff");
    if let Some(failed) = ctx
        .sync
        .engines
        .lock()
        .await
        .failed
        .get_mut("me@example.com")
    {
        failed.retry_at = tokio::time::Instant::now();
    }
    for _ in 0..2 {
        ctx.sync.wake(&ctx, None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        reconciles_done(&ctx).await;
    }
    assert_eq!(builds_of(&builds), 2, "one retry for two wakes");
}

/// The mock, with every server search waiting through one HTTP-layer retry first.
struct RetryingSearch {
    inner: MockProvider,
    observer: crate::provider::http::RetryObserver,
}

type Answer<T> = Result<T, crate::provider::ProviderError>;

#[async_trait::async_trait]
impl crate::provider::Provider for RetryingSearch {
    async fn folders(&self) -> Answer<Vec<crate::provider::RemoteFolder>> {
        self.inner.folders().await
    }
    async fn profile(&self) -> Answer<crate::provider::Profile> {
        self.inner.profile().await
    }
    async fn backfill(
        &self,
        window: &crate::provider::Window,
        page: Option<&crate::provider::PageToken>,
    ) -> Answer<crate::provider::EnvelopePage> {
        self.inner.backfill(window, page).await
    }
    async fn changes(
        &self,
        cursor: &crate::provider::SyncCursor,
    ) -> Answer<crate::provider::ChangeSet> {
        self.inner.changes(cursor).await
    }
    async fn body(&self, id: &str) -> Answer<crate::provider::Body> {
        self.inner.body(id).await
    }
    async fn search(&self, query: &str, limit: usize) -> Answer<Vec<crate::provider::RemoteId>> {
        (self.observer)(Duration::from_secs(30));
        self.inner.search(query, limit).await
    }
    async fn envelopes(
        &self,
        ids: &[crate::provider::RemoteId],
    ) -> Answer<crate::provider::EnvelopePage> {
        self.inner.envelopes(ids).await
    }
}

/// Review minor 5: a server search that retries leaves an idle account's phase unchanged
/// (only a sync step's retries show as `retrying`).
#[tokio::test]
async fn a_retrying_search_leaves_an_idle_accounts_phase_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let mock = MockProvider::new();
    let factory: super::ProviderFactory = Arc::new(move |request: super::ProviderRequest| {
        Ok(Arc::new(RetryingSearch {
            inner: mock.clone(),
            observer: request.observer,
        }) as Arc<dyn crate::provider::Provider>)
    });
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(factory));
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed >= 1 && !s.pass_running).await;
    let request = crate::ops::SearchRequest {
        query: "anything".into(),
        account: None,
        limit: 5,
    };
    let searched = tokio::time::timeout(
        Duration::from_secs(5),
        crate::sync::search::run(&ctx, request),
    )
    .await;
    assert!(searched.expect("search answered").is_ok());
    let status = ctx.sync.board().get("me@example.com").unwrap();
    assert_eq!(status.phase, SyncPhase::Idle);
    assert!(status.next_attempt_at.is_some_and(|at| at > 0));
    let observer = super::parts::retry_observer(ctx.sync.board().clone(), "me@example.com".into());
    observer(Duration::from_secs(30));
    let status = ctx.sync.board().get("me@example.com").unwrap();
    assert_eq!(
        status.phase,
        SyncPhase::Retrying,
        "a sync's retry still shows"
    );
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}
