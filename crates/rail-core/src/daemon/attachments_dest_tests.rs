//! A save's refusal of Rail's own attachment cache compares the resolved directories, not
//! their spelling: a symlink to the cache, or a `..` that walks back into it, is the cache.

use std::path::Path;

use super::attachments_tests::{attachment_calls, fixture, nothing_written, save, save_into};
use crate::error::ErrorClass;
use crate::ops::ErrorCode;

/// A save into `dir` is refused as Rail's own cache before anything is fetched.
async fn refused_as_the_cache(h: &super::draft_tests::DaemonHarness, dir: &str) {
    let before = attachment_calls(h);
    let error = save_into(h, &["0.1"], dir)
        .await
        .expect_err("Rail's own cache by another name");
    assert_eq!(error.code, ErrorCode::InvalidInput, "{dir}");
    assert_eq!(error.class, ErrorClass::Permanent, "{dir}");
    assert!(
        error.message.contains("sweeps its own attachment cache"),
        "{error}"
    );
    assert_eq!(attachment_calls(h), before, "Gmail was not asked");
}

/// A symlink outside the cache that points into it, and a path that climbs back into it with
/// `..`, are both the cache.
#[tokio::test]
async fn a_symlink_or_dot_dot_into_the_cache_is_still_the_cache() {
    let (h, dest) = fixture().await;
    let cache = h.paths.attachments_dir();
    std::fs::create_dir_all(&cache).unwrap();
    let link = h.paths.cache_dir.join("looks-elsewhere");
    std::os::unix::fs::symlink(&cache, &link).unwrap();
    refused_as_the_cache(&h, &link.display().to_string()).await;
    refused_as_the_cache(&h, &link.join("deeper").display().to_string()).await;
    let climbing = dest.join("..").join("attachments").join("mine");
    refused_as_the_cache(&h, &climbing.display().to_string()).await;
    nothing_written(&cache);
    assert!(
        !cache.join("mine").exists(),
        "the destination was created anyway"
    );
}

/// A directory that only shares the cache's name as a prefix is not the cache.
#[tokio::test]
async fn a_sibling_whose_name_starts_like_the_cache_is_not_the_cache() {
    let (h, _dest) = fixture().await;
    let sibling = h.paths.cache_dir.join("attachments-kept");
    let response = save(&h, &["0.1"], &sibling).await.expect("a sibling saves");
    assert_eq!(response.results.len(), 1);
    assert!(Path::new(&sibling).join("plan.pdf").is_file());
}

/// A `~` the client had no home for reaches the daemon as typed and is refused by name.
#[tokio::test]
async fn an_unexpanded_tilde_is_refused_as_not_absolute() {
    let (h, _dest) = fixture().await;
    for dir in ["~", "~/mail"] {
        let before = attachment_calls(&h);
        let error = save_into(&h, &["0.1"], dir)
            .await
            .expect_err("not absolute");
        assert_eq!(error.code, ErrorCode::InvalidInput, "{dir}");
        assert_eq!(error.message, "dir: not an absolute path", "{dir}");
        assert_eq!(attachment_calls(&h), before, "Gmail was not asked");
    }
}
