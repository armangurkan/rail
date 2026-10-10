//! The daemon's own methods: the handshake, its state and stopping it.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The first request on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloRequest {
    pub protocol: u32,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResponse {
    pub protocol: u32,
    /// The daemon's Rail version.
    pub version: String,
    pub pid: u32,
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
