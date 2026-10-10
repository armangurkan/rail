//! Finding the daemon, and starting it when nobody answers. An advisory lock on `rail.lock`
//! makes sure that clients starting together spawn exactly one daemon. The running daemon
//! holds `daemon.lock` for its whole life and releases it only after removing its socket, so a
//! client that finds the daemon silent waits on that lock before it clears the socket and
//! spawns a successor, unless the socket answers meanwhile: then the daemon was only starting.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UnixStream;
use tokio::sync::OnceCell;

use super::client::DaemonClient;
use super::wire::PROTOCOL_VERSION;
use crate::error::ErrorClass;
use crate::ops::op::DaemonStop;
use crate::ops::{DaemonStopRequest, ErrorCode, HelloResponse, OpError};
use crate::paths::{Paths, create_private_dir};

/// Rail's version, which the daemon and its clients must share.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long a client waits for a daemon it started.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a client waits for a daemon that stopped answering to release `daemon.lock`.
pub const DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(35);
/// `sun_path` is 104 bytes on macOS (108 on Linux); stay under the smaller.
pub const MAX_SOCKET_PATH: usize = 103;
const POLL: Duration = Duration::from_millis(50);

/// Starts a daemon process (or, in tests, an in-process server). It returns once the start is
/// under way; the connector then waits for the socket.
pub type SpawnFn = Arc<dyn Fn() -> Result<(), OpError> + Send + Sync>;

/// How a front end reaches the daemon.
#[derive(Clone)]
pub struct Connector {
    paths: Paths,
    spawn: SpawnFn,
    version: String,
    start_timeout: Duration,
    exit_timeout: Duration,
    cached: Arc<OnceCell<DaemonClient>>,
}

impl Connector {
    pub fn new(paths: Paths, spawn: SpawnFn) -> Self {
        Self {
            paths,
            spawn,
            version: VERSION.to_string(),
            start_timeout: START_TIMEOUT,
            exit_timeout: DAEMON_EXIT_TIMEOUT,
            cached: Arc::new(OnceCell::new()),
        }
    }

    /// Speak as another Rail version (tests of the mismatch path).
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_start_timeout(mut self, timeout: Duration) -> Self {
        self.start_timeout = timeout;
        self
    }

    /// How long to wait for a silent daemon to release `daemon.lock`.
    pub fn with_exit_timeout(mut self, timeout: Duration) -> Self {
        self.exit_timeout = timeout;
        self
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// One shared connection for the life of this connector (and its clones).
    pub async fn client(&self) -> Result<DaemonClient, OpError> {
        self.cached
            .get_or_try_init(|| self.connect())
            .await
            .cloned()
    }

    /// A fresh connection to a daemon of this Rail version, starting one if needed. A daemon of
    /// another version is asked to stop and replaced.
    pub async fn connect(&self) -> Result<DaemonClient, OpError> {
        for _ in 0..2 {
            let client = self.connect_or_start().await?;
            if self.matches(client.daemon()) {
                return Ok(client);
            }
            tracing::info!(
                theirs = %client.daemon().version,
                ours = %self.version,
                "replacing a daemon of another version"
            );
            self.stop(client).await?;
        }
        Err(OpError::new(
            ErrorCode::Daemon,
            ErrorClass::Permanent,
            "the daemon kept answering with another Rail version",
        )
        .with_hint("run `rail daemon stop`, then try again"))
    }

    /// A connection to the daemon if one is running; never starts one.
    pub async fn connect_existing(&self) -> Result<Option<DaemonClient>, OpError> {
        self.try_connect().await
    }

    /// Ask `client`'s daemon to stop and wait until its socket is gone.
    pub async fn stop(&self, client: DaemonClient) -> Result<(), OpError> {
        // The daemon may close the connection before it answers; either way it is stopping.
        let _ = client.call::<DaemonStop>(&DaemonStopRequest {}).await;
        drop(client);
        let deadline = tokio::time::Instant::now() + self.start_timeout;
        while tokio::time::Instant::now() < deadline {
            if !socket_answers(&self.paths.socket_file()).await {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        Err(OpError::new(
            ErrorCode::Daemon,
            ErrorClass::Transient,
            format!("the daemon did not stop within {:?}", self.start_timeout),
        ))
    }

    fn matches(&self, hello: &HelloResponse) -> bool {
        hello.protocol == PROTOCOL_VERSION && hello.version == self.version
    }

    async fn connect_or_start(&self) -> Result<DaemonClient, OpError> {
        if let Some(client) = self.try_connect().await? {
            return Ok(client);
        }
        let lock = self.lock().await?;
        if let Some(client) = self.try_connect().await? {
            return Ok(client);
        }
        // Nobody answers, but a daemon that is shutting down still holds `daemon.lock`: let it
        // finish removing its socket before we clear the path and start a successor. One that
        // is merely starting answers soon, and then it is the daemon we want.
        if let Some(client) = self.wait_for_exit_or_answer().await? {
            return Ok(client);
        }
        remove_stale_socket(&self.paths.socket_file())?;
        (self.spawn)()?;
        let client = self.wait_for_daemon().await;
        drop(lock);
        client
    }

    /// Wait until no daemon holds `daemon.lock` (`None`: go on and start one) or until the
    /// socket answers (`Some`: a daemon that was only starting). Bounded by the exit timeout.
    async fn wait_for_exit_or_answer(&self) -> Result<Option<DaemonClient>, OpError> {
        let runtime = self.paths.runtime_dir.clone();
        let path = self.paths.daemon_lock_file();
        let file = tokio::task::spawn_blocking(move || open_lock_file(&runtime, &path))
            .await
            .map_err(|e| OpError::internal(format!("lock task failed: {e}")))?
            .map_err(|e| exit_io_error(&e))?;
        let deadline = tokio::time::Instant::now() + self.exit_timeout;
        loop {
            if try_take_lock(&file)? {
                return Ok(None);
            }
            if let Some(client) = self.try_connect().await? {
                return Ok(Some(client));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(exit_timeout_error());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn try_connect(&self) -> Result<Option<DaemonClient>, OpError> {
        let socket = self.paths.socket_file();
        check_socket_path(&socket)?;
        match UnixStream::connect(&socket).await {
            Ok(stream) => match self.handshake(stream).await? {
                Ok(client) => Ok(Some(client)),
                // A socket that accepts and then hangs up is a daemon on its way out (or a
                // stale listener a forked process briefly held); treat it as absent.
                Err(e) if e.class == ErrorClass::Transient => Ok(None),
                Err(e) => Err(e),
            },
            Err(e) if is_absent(&e) => Ok(None),
            Err(e) => Err(OpError::io(
                ErrorCode::Daemon,
                "connecting to the daemon",
                &e,
            )),
        }
    }

    /// The hello, bounded: a daemon that accepts and never answers must not hang the caller.
    /// The outer error is the timeout; the inner one is the handshake's own failure.
    async fn handshake(
        &self,
        stream: UnixStream,
    ) -> Result<Result<DaemonClient, OpError>, OpError> {
        let hello = DaemonClient::handshake(stream, &self.version);
        tokio::time::timeout(self.start_timeout, hello)
            .await
            .map_err(|_| {
                OpError::new(
                    ErrorCode::Daemon,
                    ErrorClass::Transient,
                    format!(
                        "the daemon did not answer the hello within {:?}",
                        self.start_timeout
                    ),
                )
                .with_hint("run `rail daemon stop`, then try again")
            })
    }

    async fn wait_for_daemon(&self) -> Result<DaemonClient, OpError> {
        let deadline = tokio::time::Instant::now() + self.start_timeout;
        loop {
            if let Some(client) = self.try_connect().await? {
                return Ok(client);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(OpError::new(
                    ErrorCode::Daemon,
                    ErrorClass::Transient,
                    format!("the daemon did not start within {:?}", self.start_timeout),
                )
                .with_hint("see rail.log in the state directory"));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// The start lock, held until dropped. Taken on the blocking pool: `flock` blocks.
    async fn lock(&self) -> Result<File, OpError> {
        let dir = self.paths.runtime_dir.clone();
        let path = self.paths.lock_file();
        tokio::task::spawn_blocking(move || lock_file(&dir, &path))
            .await
            .map_err(|e| OpError::internal(format!("lock task failed: {e}")))?
            .map_err(|e| OpError::io(ErrorCode::Daemon, "locking the daemon start", &e))
    }
}

pub(crate) fn open_lock_file(dir: &Path, path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    create_private_dir(dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
}

fn lock_file(dir: &Path, path: &Path) -> std::io::Result<File> {
    let file = open_lock_file(dir, path)?;
    // By path: `file.lock()` would resolve to `std::fs::File::lock`, newer than our MSRV.
    fs4::FileExt::lock(&file)?;
    Ok(file)
}

fn exit_io_error(error: &std::io::Error) -> OpError {
    OpError::io(ErrorCode::Daemon, "waiting for the daemon to exit", error)
}

fn exit_timeout_error() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the running daemon did not exit in time",
    )
    .with_hint("run `rail daemon stop`, or end the rail process by hand")
}

/// Try to take `file`'s lock without blocking; true when taken. Dropping `file` releases it.
fn try_take_lock(file: &File) -> Result<bool, OpError> {
    match fs4::FileExt::try_lock(file) {
        Ok(()) => Ok(true),
        Err(fs4::TryLockError::WouldBlock) => Ok(false),
        Err(fs4::TryLockError::Error(e)) => Err(exit_io_error(&e)),
    }
}

/// Block until no daemon holds `daemon_lock` (`Paths::daemon_lock_file`), at most `timeout`:
/// take the lock and let go of it at once. A daemon that is not running holds nothing, so this
/// returns immediately. Blocking; call it from `spawn_blocking`.
pub fn wait_for_daemon_exit(daemon_lock: &Path, timeout: Duration) -> Result<(), OpError> {
    let dir = daemon_lock.parent().unwrap_or_else(|| Path::new("."));
    let file = open_lock_file(dir, daemon_lock).map_err(|e| exit_io_error(&e))?;
    let deadline = Instant::now() + timeout;
    loop {
        if try_take_lock(&file)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(exit_timeout_error());
        }
        std::thread::sleep(POLL);
    }
}

/// Nothing listens: no socket file, or a file nobody accepts on.
fn is_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

pub(crate) async fn socket_answers(socket: &Path) -> bool {
    UnixStream::connect(socket).await.is_ok()
}

/// Remove a socket file nobody listens on.
pub(crate) fn remove_stale_socket(socket: &Path) -> Result<(), OpError> {
    match std::fs::remove_file(socket) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(OpError::io(
            ErrorCode::Daemon,
            "removing a stale socket",
            &e,
        )),
    }
}

/// Unix socket paths have a small fixed limit; say so instead of failing obscurely.
pub(crate) fn check_socket_path(socket: &Path) -> Result<(), OpError> {
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(OpError::new(
            ErrorCode::Daemon,
            ErrorClass::Permanent,
            format!(
                "the socket path {} is too long for a Unix socket",
                socket.display()
            ),
        )
        .with_hint("set RAIL_RUNTIME_DIR to a shorter directory"));
    }
    Ok(())
}
