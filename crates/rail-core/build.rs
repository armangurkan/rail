//! Stamps the build time into the crate, so two builds of the same version can be told apart:
//! a daemon left running from an earlier build is then replaced by the newer client.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // `SOURCE_DATE_EPOCH` makes the build reproducible; exported for good, it gives every build
    // the same stamp, so rebuilds of one version no longer tell apart. No `rerun-if` lines:
    // Cargo's default reruns this when any file in rail-core changes (where the daemon's
    // operations live), which is what keeps the stamp fresh.
    let from_env = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let stamp = from_env.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    });
    println!("cargo:rustc-env=RAIL_BUILD={stamp}");
}
