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

fn var(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
