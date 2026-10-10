//! A connection to the daemon. Requests on one connection run concurrently: each carries an
//! id and its response is matched back to the caller. Notifications go to every subscriber.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::wire::{
    Message, Notification, PROTOCOL_VERSION, Request, Response, encode_frame, parse_message,
    read_frame,
};
use crate::error::ErrorClass;
use crate::ops::op::{Hello, Operation};
use crate::ops::{ErrorCode, HelloRequest, HelloResponse, OpError};

/// How many notifications a slow subscriber may fall behind before it misses some.
const NOTIFICATION_BUFFER: usize = 64;

/// A handshaken connection. Cheap to clone; the connection closes when the last clone drops.
#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<Inner>,
    hello: Arc<HelloResponse>,
}

#[derive(Default)]
struct Waiting {
    closed: bool,
    /// Why the connection ended, when it ended on an error rather than a clean close.
    failure: Option<OpError>,
    calls: HashMap<u64, oneshot::Sender<Response>>,
}

struct Inner {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    waiting: Arc<Mutex<Waiting>>,
    next_id: AtomicU64,
    notifications: broadcast::Sender<Notification>,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// The daemon's answer did not fit the type we expected. Our words in `message`, serde's text
/// (which can quote what the daemon sent) in `detail`.
fn unparsable(what: &str, error: &serde_json::Error) -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Permanent,
        format!("the daemon's answer to {what} did not parse"),
    )
    .with_detail(error.to_string())
}

impl DaemonClient {
    /// Start reading and writing on `stream` and say hello with `version`.
    pub async fn handshake(stream: UnixStream, version: &str) -> Result<Self, OpError> {
        let inner = Arc::new(start(stream));
        let hello = HelloRequest {
            protocol: PROTOCOL_VERSION,
            version: version.to_string(),
        };
        let params = serde_json::to_value(&hello).map_err(|e| OpError::internal(e.to_string()))?;
        let answer = call_raw(&inner, Hello::NAME, params).await?;
        let hello: HelloResponse =
            serde_json::from_value(answer).map_err(|e| unparsable(Hello::NAME, &e))?;
        Ok(Self {
            inner,
            hello: Arc::new(hello),
        })
    }

    /// True once the connection has ended: every call on it fails.
    pub fn is_closed(&self) -> bool {
        state(&self.inner.waiting).closed
    }

    /// What the daemon said in the handshake.
    pub fn daemon(&self) -> &HelloResponse {
        &self.hello
    }

    /// Run operation `O` in the daemon.
    pub async fn call<O: Operation>(&self, request: &O::Request) -> Result<O::Response, OpError> {
        let params = serde_json::to_value(request).map_err(|e| OpError::internal(e.to_string()))?;
        let result = call_raw(&self.inner, O::NAME, params).await?;
        serde_json::from_value(result).map_err(|e| unparsable(O::NAME, &e))
    }

    /// Every notification the daemon sends on this connection from now on.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.inner.notifications.subscribe()
    }
}

fn start(stream: UnixStream) -> Inner {
    let (read, write) = stream.into_split();
    let (frames, outgoing) = mpsc::unbounded_channel();
    let waiting = Arc::new(Mutex::new(Waiting::default()));
    let (notifications, _) = broadcast::channel(NOTIFICATION_BUFFER);
    let reader = tokio::spawn(read_loop(read, waiting.clone(), notifications.clone()));
    let writer = tokio::spawn(write_loop(write, outgoing));
    Inner {
        frames,
        waiting,
        next_id: AtomicU64::new(1),
        notifications,
        tasks: [reader, writer],
    }
}

fn disconnected() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the daemon closed the connection",
    )
}

/// The waiting calls, usable even if a panicking thread poisoned the lock.
fn state(waiting: &Mutex<Waiting>) -> MutexGuard<'_, Waiting> {
    waiting.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why calls on a closed connection fail: the error that ended it, else a plain close.
fn closed_error(waiting: &Mutex<Waiting>) -> OpError {
    state(waiting).failure.clone().unwrap_or_else(disconnected)
}

async fn call_raw(inner: &Inner, method: &str, params: Value) -> Result<Value, OpError> {
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let frame = encode_frame(&Request::new(id, method, params))?;
    let (tx, rx) = oneshot::channel();
    {
        let mut waiting = state(&inner.waiting);
        if waiting.closed {
            return Err(waiting.failure.clone().unwrap_or_else(disconnected));
        }
        waiting.calls.insert(id, tx);
    }
    if inner.frames.send(frame).is_err() {
        state(&inner.waiting).calls.remove(&id);
        return Err(closed_error(&inner.waiting));
    }
    match rx.await {
        Ok(response) => response.into_result(),
        Err(_) => Err(closed_error(&inner.waiting)),
    }
}

async fn read_loop(
    read: OwnedReadHalf,
    waiting: Arc<Mutex<Waiting>>,
    notifications: broadcast::Sender<Notification>,
) {
    let mut reader = BufReader::new(read);
    let failure = loop {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break None,
            Err(error) => {
                tracing::warn!(%error, "the daemon connection failed while reading");
                break Some(OpError::from(error));
            }
        };
        match parse_message(&frame) {
            Ok(Message::Response(response)) => {
                let caller = response.id.and_then(|id| state(&waiting).calls.remove(&id));
                if let Some(caller) = caller {
                    let _ = caller.send(response);
                }
            }
            Ok(Message::Notification(notification)) => {
                // No subscriber is fine: nobody asked to hear about it.
                let _ = notifications.send(notification);
            }
            Ok(Message::Request(_)) | Err(_) => {
                tracing::debug!("ignoring an unexpected message from the daemon");
            }
        }
    };
    let mut waiting = state(&waiting);
    waiting.closed = true;
    waiting.failure = failure;
    // Dropping the senders wakes every caller still waiting; they read `failure`.
    waiting.calls.clear();
}

async fn write_loop(mut write: OwnedWriteHalf, mut frames: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = frames.recv().await {
        if write.write_all(&frame).await.is_err() {
            break;
        }
    }
    let _ = write.shutdown().await;
}
