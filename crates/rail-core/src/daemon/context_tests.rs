use std::sync::Arc;
use std::time::Duration;

use crate::config::lock::lock_config;
use crate::paths::Paths;
use crate::test_support::daemon_context;

/// Long enough for an unguarded write to finish many times over.
const SETTLE: Duration = Duration::from_millis(200);

#[tokio::test]
async fn a_config_write_waits_while_another_process_holds_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let ctx = Arc::new(daemon_context(&paths).0);
    let held = lock_config(&paths.config_file()).unwrap();
    let write = tokio::spawn({
        let ctx = ctx.clone();
        async move { ctx.write_locked(|_| Ok(())).await }
    });
    tokio::time::sleep(SETTLE).await;
    assert!(!write.is_finished(), "the write waits for the lock");
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), write)
        .await
        .expect("the write goes ahead once the lock is free")
        .unwrap()
        .unwrap();
}
