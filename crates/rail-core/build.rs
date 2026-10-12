//! Stamps the build time into the crate, so two builds of the same version can be told apart:
//! a daemon left running from an earlier build is then replaced by the newer client.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // `SOURCE_DATE_EPOCH` makes the build reproducible; exported for good, it gives every build
    // the same stamp, so rebuilds of one version no longer tell apart. The `rerun-if` lines keep
    // Cargo's default of rerunning this when a file in rail-core changes (where the daemon's
    // operations live), which keeps the stamp fresh, and add the environment variable. The stamp
    // is the time rail-core was last built, not the time of the final link.
    let from_env = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let stamp = from_env.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    });
    for line in [
        "cargo:rerun-if-changed=src",
        "cargo:rerun-if-changed=build.rs",
        "cargo:rerun-if-changed=Cargo.toml",
        "cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH",
    ] {
        println!("{line}");
    }
    println!("cargo:rustc-env=RAIL_BUILD={stamp}");
}
