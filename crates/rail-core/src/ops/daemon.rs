//! The daemon's own methods: the handshake, its state and stopping it.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The first request on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloRequest {
    pub protocol: u32,
    pub version: String,
    /// The client's display and browser, for browser launches this connection begins.
    #[serde(default)]
    pub browser: BrowserEnv,
}

/// The longest display or browser value the daemon takes from a client.
pub const MAX_BROWSER_ENV_CHARS: usize = 4096;

/// Where a client's user sees a browser: its `DISPLAY`, `WAYLAND_DISPLAY` and `BROWSER`.
/// Used only to launch the browser for a login that client began; never the daemon's own
/// environment, and never from an MCP tool argument.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserEnv {
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub wayland_display: Option<String>,
    #[serde(default)]
    pub browser: Option<String>,
}

impl BrowserEnv {
    /// This process's values; empty ones count as unset.
    pub fn from_process() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            display: var("DISPLAY"),
            wayland_display: var("WAYLAND_DISPLAY"),
            browser: var("BROWSER"),
        }
    }

    /// Drop any value that is too long or holds a control character.
    pub fn checked(self) -> Self {
        let fine = |value: Option<String>| {
            value.filter(|v| {
                v.chars().count() <= MAX_BROWSER_ENV_CHARS && !v.chars().any(char::is_control)
            })
        };
        Self {
            display: fine(self.display),
            wayland_display: fine(self.wayland_display),
            browser: fine(self.browser),
        }
    }

    /// The variables to set for the launch.
    pub fn vars(&self) -> Vec<(&'static str, &str)> {
        [
            ("DISPLAY", &self.display),
            ("WAYLAND_DISPLAY", &self.wayland_display),
            ("BROWSER", &self.browser),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_deref().map(|v| (key, v)))
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResponse {
    pub protocol: u32,
    /// The daemon's Rail version.
    pub version: String,
    pub pid: u32,
    /// When the daemon's Rail was built, in unix seconds; orders two builds of one version.
    /// `None` from a daemon older than the field, which a client treats as the oldest build.
    #[serde(default)]
    pub build: Option<u64>,
    /// The directories the daemon works on. A client whose own differ must not use it: it
    /// would read and write another configuration than the one it thinks it has. Empty when an
    /// older daemon did not send them; the client then cannot tell, and treats it as a mismatch
    /// for ordinary requests while still being able to stop or replace it.
    #[serde(default)]
    pub config_dir: PathBuf,
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfoRequest {}

/// What the daemon is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub socket: PathBuf,
    /// Unix seconds.
    pub started_at: i64,
    /// Connected clients, including the one asking.
    pub clients: usize,
    /// Logins and consents waiting on a browser or saving.
    pub pending: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonStopRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonStopResponse {
    pub stopping: bool,
}
