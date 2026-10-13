//! The provider and the engine share one window: a provider is only ever built beside the
//! engine it serves, from the same `config.toml`, so an edited `history_days` (in `[general]`
//! or on the account) restarts both together and they never disagree.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::daemon::DaemonContext;
use crate::paths::Paths;
use crate::provider::Provider;
use crate::provider::mock::MockProvider;
use crate::sync::providers::window_days;
use crate::test_support::daemon_context;

const ADDRESS: &str = "me@corp.example";

fn config(general_days: u32, account_days: Option<u32>) -> String {
    let own = account_days.map_or(String::new(), |d| format!("history_days = {d}\n"));
    format!(
        "[general]\nsecrets = \"file\"\npoll_interval = \"60s\"\nhistory_days = {general_days}\n\n\
         [[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"{ADDRESS}\"\nclient_id = \"cid\"\n{own}"
    )
}

/// The provider's window, then the running engine's, once both report the same build.
async fn windows(ctx: &DaemonContext, built: &Mutex<Vec<u32>>, builds: usize) -> (u32, u32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let provider = built.lock().unwrap().get(builds - 1).copied();
            let engine = ctx
                .sync
                .engines
                .lock()
                .await
                .running
                .get(ADDRESS)
                .map(|r| r.key.history_days());
            if let (Some(provider), Some(engine)) = (provider, engine) {
                return (provider, engine);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the account's engine started")
}

#[tokio::test]
async fn an_edited_window_rebuilds_the_provider_with_the_engines_window() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config(30, None)).unwrap();
    let built = Arc::new(Mutex::new(Vec::new()));
    let (seen, mock) = (built.clone(), MockProvider::new());
    let factory: crate::sync::ProviderFactory = Arc::new(move |request| {
        seen.lock().unwrap().push(window_days(&request));
        Ok(Arc::new(mock.clone()) as Arc<dyn Provider>)
    });
    let (ctx, _) = daemon_context(&paths);
    let ctx = Arc::new(ctx.with_provider_factory(factory));

    ctx.sync.reconcile(&ctx).await;
    assert_eq!(windows(&ctx, &built, 1).await, (30, 30));

    std::fs::write(paths.config_file(), config(90, None)).unwrap();
    ctx.sync.reconcile(&ctx).await;
    assert_eq!(windows(&ctx, &built, 2).await, (90, 90), "[general] edited");

    std::fs::write(paths.config_file(), config(90, Some(7))).unwrap();
    ctx.sync.reconcile(&ctx).await;
    assert_eq!(
        windows(&ctx, &built, 3).await,
        (7, 7),
        "the account's own window"
    );

    ctx.sync.reconcile(&ctx).await;
    assert_eq!(
        built.lock().unwrap().len(),
        3,
        "an unchanged window builds nothing"
    );
    ctx.sync.shutdown(Duration::from_secs(5)).await;
}
