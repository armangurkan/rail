//! The daemon's listener: the socket and its modes, the peer-uid check, the idle exit and the
//! stop request.

use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

use super::connect::{
    VERSION, check_socket_path, open_lock_file, remove_stale_socket, socket_answers,
};
use super::connection::Connection;
use super::context::DaemonContext;
use super::handlers;
use super::registry::Registry;
use super::wire::PROTOCOL_VERSION;
use crate::auth::unix_now;
use crate::error::ErrorClass;
use crate::ops::{DaemonInfo, ErrorCode, OpError};
use crate::paths::{Paths, create_private_dir};

/// How long the daemon waits with no client and nothing pending before it exits.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub idle: Duration,
    /// The Rail version the daemon announces in the hello.
    pub version: String,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            idle: IDLE_TIMEOUT,
            version: VERSION.to_string(),
        }
    }
}

/// Shared by the accept loop, every connection and the handlers.
pub(crate) struct ServerState {
    pub(crate) config: ServeConfig,
    clients: AtomicUsize,
    last_active: Mutex<Instant>,
    shutdown: Notify,
    started_at: i64,
    socket: PathBuf,
}

impl ServerState {
    pub(crate) fn client_connected(&self) {
        self.clients.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    pub(crate) fn client_disconnected(&self) {
        self.clients.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    pub(crate) fn touch(&self) {
        *self.last_active.lock().expect("activity lock") = Instant::now();
    }

    pub(crate) fn request_shutdown(&self) {
        self.shutdown.notify_one();
    }

    pub(crate) fn info(&self, ctx: &DaemonContext) -> DaemonInfo {
        DaemonInfo {
            pid: std::process::id(),
            version: self.config.version.clone(),
            protocol: PROTOCOL_VERSION,
            socket: self.socket.clone(),
            started_at: self.started_at,
            clients: self.clients.load(Ordering::SeqCst),
            pending: ctx.pending_count(),
        }
    }

    fn idle_expired(&self, ctx: &DaemonContext) -> bool {
        let quiet = self.last_active.lock().expect("activity lock").elapsed();
        self.clients.load(Ordering::SeqCst) == 0
            && ctx.pending_count() == 0
            && quiet >= self.config.idle
    }
}

/// True when a connection from `peer_uid` may talk to a daemon running as `own_uid`.
pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

pub struct Server {
    listener: UnixListener,
    ctx: Arc<DaemonContext>,
    registry: Arc<Registry>,
    state: Arc<ServerState>,
    own_uid: u32,
    /// `daemon.lock`, held for the daemon's whole life and released last, after the socket is
    /// gone, so a client that waits on it never meets a half-removed daemon.
    lifetime_lock: File,
}

impl Server {
    /// Create the runtime directory (0700), take `daemon.lock` (refusing, without touching the
    /// socket, if another daemon holds it), replace a stale socket, bind and make the socket
    /// 0600. Nobody else can reach it in between: the directory is the owner's alone.
    pub async fn bind(ctx: DaemonContext, config: ServeConfig) -> Result<Server, OpError> {
        let socket = ctx.paths.socket_file();
        check_socket_path(&socket)?;
        let dir = &ctx.paths.runtime_dir;
        let io = |what: &str, e: std::io::Error| OpError::io(ErrorCode::Daemon, what, &e);
        create_private_dir(dir).map_err(|e| io("creating the runtime directory", e))?;
        let own_uid = std::fs::metadata(dir)
            .map_err(|e| io("reading the runtime directory", e))?
            .uid();
        let lifetime_lock = take_lifetime_lock(&ctx.paths, &socket)?;
        if socket_answers(&socket).await {
            return Err(already_running(&socket));
        }
        remove_stale_socket(&socket)?;
        let listener = UnixListener::bind(&socket).map_err(|e| io("binding the socket", e))?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io("restricting the socket", e))?;
        let state = ServerState {
            config,
            clients: AtomicUsize::new(0),
            last_active: Mutex::new(Instant::now()),
            shutdown: Notify::new(),
            started_at: unix_now(),
            socket,
        };
        Ok(Server {
            listener,
            ctx: Arc::new(ctx),
            registry: Arc::new(handlers::registry()),
            state: Arc::new(state),
            own_uid,
            lifetime_lock,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.state.socket
    }

    /// Serve until `daemon.stop` or the idle timeout, then remove the socket.
    pub async fn run(self) -> Result<(), OpError> {
        tracing::info!(socket = %self.state.socket.display(), "daemon listening");
        let tick = check_interval(self.state.config.idle);
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => self.admit(stream),
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                },
                () = self.state.shutdown.notified() => break,
                () = tokio::time::sleep(tick) => {
                    if self.state.idle_expired(&self.ctx) {
                        tracing::info!("no clients and nothing pending; exiting");
                        break;
                    }
                }
            }
        }
        self.finish().await
    }

    fn admit(&self, stream: UnixStream) {
        let peer = stream.peer_cred().map(|cred| cred.uid());
        match peer {
            Ok(uid) if peer_allowed(uid, self.own_uid) => {}
            Ok(uid) => {
                tracing::warn!(uid, "refusing a connection from another user");
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "refusing a connection whose peer is unknown");
                return;
            }
        }
        let connection = Connection {
            ctx: self.ctx.clone(),
            registry: self.registry.clone(),
            server: self.state.clone(),
        };
        tokio::spawn(connection.serve(stream));
    }

    /// Shut down in the one order that matters: stop listening, remove the socket, and only
    /// then release `daemon.lock`.
    async fn finish(self) -> Result<(), OpError> {
        drop(self.listener);
        let removed = remove_stale_socket(&self.state.socket);
        drop(self.lifetime_lock);
        tracing::info!("daemon stopped");
        removed
    }
}

fn already_running(socket: &Path) -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Permanent,
        format!("another rail daemon is running on {}", socket.display()),
    )
}

/// Take `daemon.lock` without blocking. By path: `file.try_lock()` would resolve to std's
/// `File::try_lock`, newer than our MSRV.
fn take_lifetime_lock(paths: &Paths, socket: &Path) -> Result<File, OpError> {
    let io = |e: std::io::Error| OpError::io(ErrorCode::Daemon, "locking the daemon", &e);
    let file = open_lock_file(&paths.runtime_dir, &paths.daemon_lock_file()).map_err(io)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(already_running(socket)),
        Err(fs4::TryLockError::Error(e)) => Err(io(e)),
    }
}

/// How often the idle timer looks: a tenth of the timeout, between 10 ms and 5 s.
fn check_interval(idle: Duration) -> Duration {
    (idle / 10).clamp(Duration::from_millis(10), Duration::from_secs(5))
}
