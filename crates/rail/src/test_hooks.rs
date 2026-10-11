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

/// Let a test daemon idle out even with accounts configured.
pub fn idle_with_accounts() -> bool {
    var("RAIL_TEST_IDLE_WITH_ACCOUNTS").is_some()
}

fn var(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
