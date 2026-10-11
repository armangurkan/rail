//! Environment hooks for the binary tests. Honoured in debug builds only: a release build
//! ignores them, so no environment variable can redirect a real user's login.

/// A program run instead of `xdg-open`/`open`, with the URL as its only argument.
pub fn opener() -> Option<String> {
    var("RAIL_TEST_OPENER")
}

/// A token endpoint used instead of the provider's, so a test can serve it with wiremock.
pub fn token_url() -> Option<String> {
    var("RAIL_TEST_TOKEN_URL")
}

/// How long the daemon waits idle before it exits, in seconds, instead of ten minutes, so the
/// daemons the binary tests start do not outlive them.
pub fn idle() -> Option<std::time::Duration> {
    var("RAIL_TEST_IDLE_SECS")
        .and_then(|secs| secs.parse().ok())
        .map(std::time::Duration::from_secs)
}

/// The Gmail API root, so a binary test can serve it with wiremock (or make it unreachable).
pub fn gmail_url() -> Option<String> {
    var("RAIL_TEST_GMAIL_URL")
}

/// How long `rail sync` watches with no change before it leaves the sync to the daemon,
/// instead of ten minutes.
pub fn sync_stall() -> Option<std::time::Duration> {
    var("RAIL_TEST_SYNC_STALL_SECS")
        .and_then(|secs| secs.parse().ok())
        .map(std::time::Duration::from_secs)
}

/// Let a test daemon idle out even with accounts configured.
pub fn idle_with_accounts() -> bool {
    var("RAIL_TEST_IDLE_WITH_ACCOUNTS").is_some()
}

/// A file that receives the service-manager commands (`systemctl`, `launchctl`) instead of
/// running them, one line each, so no test ever reaches the real service manager.
pub fn service_log() -> Option<std::path::PathBuf> {
    var("RAIL_TEST_SERVICE_LOG").map(Into::into)
}

/// Make the mail screen panic on its first key, so a binary test can check that the terminal
/// is restored.
pub fn mail_panic() -> bool {
    var("RAIL_TEST_MAIL_PANIC").is_some()
}

/// How long `run_app` waits after the terminal is restored, in seconds, so a binary test can
/// signal Rail while it is past its run loop (as during the wizard's settle).
pub fn linger_after_loop() -> Option<std::time::Duration> {
    var("RAIL_TEST_LINGER_SECS")
        .and_then(|secs| secs.parse().ok())
        .map(std::time::Duration::from_secs)
}

fn var(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
