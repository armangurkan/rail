//! The background daemon that owns the store, the token sources, the loopback listener and
//! every secret-store call, and its clients. Front ends send requests; only the daemon
//! executes operations.

pub mod client;
pub mod connect;
mod connection;
pub mod context;
mod handlers;
mod pending;
mod registry;
pub mod server;
pub mod wire;

#[cfg(test)]
mod connect_tests;
#[cfg(test)]
mod login_tests;
#[cfg(test)]
mod tests;

pub use client::DaemonClient;
pub use connect::{
    Connector, DAEMON_EXIT_TIMEOUT, MAX_SOCKET_PATH, START_TIMEOUT, SpawnFn, VERSION,
    wait_for_daemon_exit,
};
pub use context::{DaemonContext, Opener};
pub use server::{IDLE_TIMEOUT, SAVE_GRACE, ServeConfig, Server, peer_allowed};
pub use wire::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
