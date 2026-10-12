//! The daemon's listener: the socket and its modes, the peer-uid check, the idle exit and the
//! stop request.

use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at};

use super::connect::{
    BUILD, START_TIMEOUT, VERSION, check_socket_path, open_lock_file, remove_stale_socket,
    socket_answers,
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
use crate::sync::{CONFIG_CHECK, SyncManager};

/// The longest `daemon.stop` (or an idle exit) waits for running saves.
pub const SAVE_GRACE: Duration = Duration::from_secs(30);

/// How long the daemon waits with no client and nothing pending before it exits.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub idle: Duration,
    /// How long a new connection may stay silent before it is closed.
    pub hello_timeout: Duration,
    /// The Rail version the daemon announces in the hello.
    pub version: String,
    /// The build (unix seconds) the daemon announces in the hello.
    pub build: u64,
    /// Let the daemon idle out even with accounts configured; a debug-build test hook.
    pub idle_with_accounts: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            idle: IDLE_TIMEOUT,
            hello_timeout: START_TIMEOUT,
            version: VERSION.to_string(),
            build: BUILD,
            idle_with_accounts: false,
        }
    }
}

/// How long `finish` waits for pending tasks to end after cancelling them.
const TASK_JOIN: Duration = Duration::from_secs(5);

/// The whole of `finish`, under `DAEMON_EXIT_TIMEOUT` (35 s), which `rail daemon stop` waits
/// for the daemon to exit: sync's shutdown and the running saves together get at most
/// `SAVE_GRACE` (30 s); the pending tasks get what is left of `TASK_JOIN` before the last
/// `CLOSE_GRACE` (3 s), which the connections get. So 30 + 3 = 33 s at worst, then removing
/// the socket, with 2 s to spare.
const FINISH_BUDGET: Duration = Duration::from_secs(33);
const _: () = assert!(SAVE_GRACE.as_secs() + CLOSE_GRACE.as_secs() <= FINISH_BUDGET.as_secs());
const _: () = assert!(FINISH_BUDGET.as_secs() < crate::daemon::DAEMON_EXIT_TIMEOUT.as_secs());

/// How long `finish` lets connections answer their last requests and flush before aborting.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// Pause after a failed accept (descriptor exhaustion, say) so the loop cannot spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Shared by the accept loop, every connection and the handlers.
pub(crate) struct ServerState {
    pub(crate) config: ServeConfig,
    clients: AtomicUsize,
    /// Milliseconds since `epoch` at the last activity.
    last_active_ms: AtomicU64,
    epoch: Instant,
    #[cfg(test)]
    max_tasks: AtomicUsize,
    shutdown: Notify,
    /// Set once shutdown starts closing connections.
    closing: tokio::sync::watch::Sender<bool>,
    started_at: i64,
    socket: PathBuf,
}

impl ServerState {
    /// Counted when the connection is accepted, before its task starts, so the idle timer
    /// never sees zero clients for a connection that is already on its way.
    pub(crate) fn client_connected(&self) {
        self.clients.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    pub(crate) fn client_disconnected(&self) {
        self.clients.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    pub(crate) fn touch(&self) {
        self.last_active_ms.store(self.now_ms(), Ordering::SeqCst);
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The most request tasks any connection held at once (tests of the drain).
    #[cfg(test)]
    pub(crate) fn note_tasks(&self, len: usize) {
        self.max_tasks.fetch_max(len, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn max_tasks(&self) -> usize {
        self.max_tasks.load(Ordering::SeqCst)
    }

    fn begin_close(&self) {
        self.closing.send_replace(true);
    }

    pub(crate) fn is_closing(&self) -> bool {
        *self.closing.borrow()
    }

    /// Resolves once shutdown starts closing connections.
    pub(crate) async fn closed(&self) {
        let mut closing = self.closing.subscribe();
        if closing.wait_for(|closing| *closing).await.is_err() {
            std::future::pending::<()>().await;
        }
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
        let since = self
            .now_ms()
            .saturating_sub(self.last_active_ms.load(Ordering::SeqCst));
        let quiet = Duration::from_millis(since);
        self.clients.load(Ordering::SeqCst) == 0
            && ctx.pending_count() == 0
            && quiet >= self.config.idle
            && (self.config.idle_with_accounts || !ctx.sync.has_accounts())
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
    /// Every live connection task, so shutdown can end them all.
    connections: JoinSet<()>,
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
        let lifetime_lock = take_lifetime_lock(&ctx.paths, &socket).await?;
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
            last_active_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            #[cfg(test)]
            max_tasks: AtomicUsize::new(0),
            shutdown: Notify::new(),
            closing: tokio::sync::watch::Sender::new(false),
            started_at: unix_now(),
            socket,
        };
        Ok(Server {
            listener,
            ctx: Arc::new(ctx),
            registry: Arc::new(handlers::registry()),
            state: Arc::new(state),
            connections: JoinSet::new(),
            own_uid,
            lifetime_lock,
        })
    }

    #[cfg(test)]
    pub(crate) fn state_for_tests(&self) -> Arc<ServerState> {
        self.state.clone()
    }

    pub fn socket(&self) -> &Path {
        &self.state.socket
    }

    /// Serve until `daemon.stop` or the idle timeout, then remove the socket.
    pub async fn run(self) -> Result<(), OpError> {
        self.run_until(std::future::pending()).await
    }

    /// `run`, also stopping, through the same `finish`, once `stop` resolves (the binary passes
    /// SIGINT and SIGTERM).
    pub async fn run_until(
        mut self,
        stop: impl std::future::Future<Output = ()>,
    ) -> Result<(), OpError> {
        tracing::info!(socket = %self.state.socket.display(), "daemon listening");
        let tick = check_interval(self.state.config.idle);
        let mut stop = std::pin::pin!(stop);
        self.ctx.sync.count_accounts(&self.ctx.paths);
        SyncManager::reconcile_in_background(self.ctx.clone());
        // Start-up housekeeping: the files of imported drafts no draft names any more.
        tokio::spawn(super::handlers::draft_files::sweep_at_start(
            self.ctx.clone(),
        ));
        // Intervals, not a sleep re-armed each turn: another arm firing more often than the
        // idle check would otherwise keep re-arming it, and the daemon would never idle out.
        // The first check waits one `CONFIG_CHECK`: the startup reconcile above covers now,
        // and a second one at once would plan the same accounts (two 1Password prompts).
        let mut config_tick = interval_at(Instant::now() + CONFIG_CHECK, CONFIG_CHECK);
        config_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut idle_tick = interval(tick);
        idle_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = &mut stop => {
                    tracing::info!("asked to stop by a signal");
                    break;
                }
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => self.admit(stream),
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                () = self.state.shutdown.notified() => break,
                _ = config_tick.tick() => SyncManager::tick(&self.ctx),
                _ = idle_tick.tick() => {
                    if self.state.idle_expired(&self.ctx) {
                        tracing::info!("no clients and nothing pending; exiting");
                        break;
                    }
                }
            }
        }
        self.finish().await
    }

    fn admit(&mut self, stream: UnixStream) {
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
        // Reap the finished ones, or a long-lived daemon grows with every client it ever had.
        while self.connections.try_join_next().is_some() {}
        self.state.client_connected();
        self.connections.spawn(connection.serve(stream));
    }

    /// The single shutdown point, in the one order that matters: stop listening; refuse new
    /// logins; stop the sync engines and let the writer commit its batch; end the logins
    /// still waiting for a browser; give running saves their time to finish; end every
    /// connection so none serves a request after this; remove the socket; and only then
    /// release `daemon.lock`.
    async fn finish(mut self) -> Result<(), OpError> {
        let deadline = Instant::now() + FINISH_BUDGET;
        let left = |reserve: Duration| {
            deadline
                .saturating_duration_since(Instant::now())
                .saturating_sub(reserve)
        };
        drop(self.listener);
        self.ctx.pending.stop_new();
        self.ctx.pending.cancel_waiting();
        // The sync writer and the saves of logins write different things: they finish side
        // by side, inside one grace.
        tokio::join!(
            self.ctx.sync.shutdown(SAVE_GRACE),
            self.ctx.pending.wait_for_saves(SAVE_GRACE)
        );
        self.ctx
            .pending
            .join_tasks(TASK_JOIN.min(left(CLOSE_GRACE)))
            .await;
        // Connections stop reading, let their requests answer and flush what is queued (the
        // reply to `daemon.stop` among it); whatever is slower than that is aborted.
        self.state.begin_close();
        let drained = tokio::time::timeout(CLOSE_GRACE.min(left(Duration::ZERO)), async {
            while self.connections.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            tracing::warn!("connections still open after the close grace; aborting them");
        }
        self.connections.abort_all();
        while self.connections.join_next().await.is_some() {}
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

/// How long a held `daemon.lock` is waited for, and how often it is tried. A process forked
/// by a thread of the previous holder (an `op` call, say) carries the lock until its `exec`, so
/// a lock that is held for a few milliseconds is not another daemon.
const LOCK_RETRIES: u32 = 20;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);

/// Take `daemon.lock` without blocking the thread. By path: `file.try_lock()` would resolve to
/// std's `File::try_lock`, newer than our MSRV.
async fn take_lifetime_lock(paths: &Paths, socket: &Path) -> Result<File, OpError> {
    let io = |e: std::io::Error| OpError::io(ErrorCode::Daemon, "locking the daemon", &e);
    let file = open_lock_file(&paths.runtime_dir, &paths.daemon_lock_file()).map_err(io)?;
    for _ in 0..LOCK_RETRIES {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(file),
            Err(fs4::TryLockError::WouldBlock) => tokio::time::sleep(LOCK_RETRY_DELAY).await,
            Err(fs4::TryLockError::Error(e)) => return Err(io(e)),
        }
    }
    Err(already_running(socket))
}

/// How often the idle timer looks: a tenth of the timeout, between 10 ms and 5 s.
fn check_interval(idle: Duration) -> Duration {
    (idle / 10).clamp(Duration::from_millis(10), Duration::from_secs(5))
}
