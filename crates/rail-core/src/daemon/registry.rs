//! Method name to handler. Each handler takes the typed request of its `Operation` and returns
//! the typed response; the registry does the JSON on either side.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::context::DaemonContext;
use super::server::ServerState;
use super::wire::{
    INVALID_PARAMS, METHOD_NOT_FOUND, Notification, Request, Response, RpcError, encode_frame,
};
use crate::ops::op::Operation;
use crate::ops::{BrowserEnv, OpError};

type Reply = Pin<Box<dyn Future<Output = Result<Value, OpError>> + Send>>;
type Handler = Box<dyn Fn(Call, Value) -> Result<Reply, OpError> + Send + Sync>;

/// Everything a handler may use: the context, the server's own state and the connection that
/// asked.
#[derive(Clone)]
pub(crate) struct Call {
    pub ctx: Arc<DaemonContext>,
    pub server: Arc<ServerState>,
    pub caller: Caller,
}

/// The connection a request came from, for notifications.
#[derive(Clone)]
pub(crate) struct Caller {
    /// Weak, so a detached pending task never keeps a closed connection's writer (and socket)
    /// alive.
    frames: mpsc::WeakUnboundedSender<Vec<u8>>,
    /// The display and browser the connection's hello named (checked).
    browser: BrowserEnv,
    /// The id of the connection's live `sync.progress` subscriber; 0 when it has none.
    /// Shared by every request of one connection.
    watching: Arc<AtomicU64>,
}

impl Caller {
    pub(crate) fn new(frames: mpsc::UnboundedSender<Vec<u8>>, browser: BrowserEnv) -> Self {
        Self {
            frames: frames.downgrade(),
            browser,
            watching: Arc::default(),
        }
    }

    /// This caller, sharing the connection's `sync.progress` watch.
    pub(crate) fn sharing_watch(mut self, watching: Arc<AtomicU64>) -> Self {
        self.watching = watching;
        self
    }

    pub(crate) fn watching(&self) -> &AtomicU64 {
        &self.watching
    }

    pub(crate) fn browser(&self) -> &BrowserEnv {
        &self.browser
    }

    /// False once the connection has gone.
    pub(crate) fn is_open(&self) -> bool {
        self.frames.upgrade().is_some()
    }

    /// Send a notification. A connection that has gone, or a frame that cannot be encoded, is
    /// logged (method only, never the content) and the operation carries on.
    pub(crate) fn notify(&self, method: &str, params: &impl Serialize) {
        let frame = serde_json::to_value(params)
            .map_err(|e| e.to_string())
            .and_then(|params| {
                encode_frame(&Notification::new(method, params)).map_err(|e| e.to_string())
            });
        match frame {
            Ok(frame) => match self.frames.upgrade() {
                Some(frames) if frames.send(frame).is_ok() => {}
                _ => tracing::debug!(method, "the client is gone; notification dropped"),
            },
            Err(error) => tracing::warn!(method, error, "could not encode a notification"),
        }
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    handlers: HashMap<&'static str, Handler>,
}

impl Registry {
    pub(crate) fn register<O, F, Fut>(&mut self, handler: F)
    where
        O: Operation,
        F: Fn(Call, O::Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O::Response, OpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let wrapped: Handler = Box::new(move |call, params| {
            let request: O::Request =
                serde_json::from_value(params).map_err(|e| OpError::invalid_params(&e))?;
            let handler = handler.clone();
            Ok(Box::pin(async move {
                let response = handler(call, request).await?;
                serde_json::to_value(response).map_err(|e| OpError::internal(e.to_string()))
            }))
        });
        self.handlers.insert(O::NAME, wrapped);
    }

    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.handlers.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Run the handler for `request` and build its response. Never fails: every problem is
    /// a JSON-RPC error in the response.
    pub(crate) async fn dispatch(&self, call: Call, request: Request) -> Response {
        let Some(handler) = self.handlers.get(request.method.as_str()) else {
            let message = format!("no operation named {:?}", request.method);
            return Response::err(Some(request.id), RpcError::new(METHOD_NOT_FOUND, message));
        };
        let params = match request.params {
            Value::Null => json!({}),
            params => params,
        };
        let reply = match handler(call, params) {
            Ok(reply) => reply,
            Err(error) => {
                let mut rpc = RpcError::from_op(&error);
                rpc.code = INVALID_PARAMS;
                return Response::err(Some(request.id), rpc);
            }
        };
        match reply.await {
            Ok(result) => Response::ok(request.id, result),
            Err(error) => Response::err(Some(request.id), RpcError::from_op(&error)),
        }
    }
}
