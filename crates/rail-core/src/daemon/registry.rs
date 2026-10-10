//! Method name to handler. Each handler takes the typed request of its `Operation` and returns
//! the typed response; the registry does the JSON on either side.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use super::context::DaemonContext;
use super::server::ServerState;
use super::wire::{INVALID_PARAMS, METHOD_NOT_FOUND, Request, Response, RpcError};
use crate::ops::OpError;
use crate::ops::op::Operation;

type Reply = Pin<Box<dyn Future<Output = Result<Value, OpError>> + Send>>;
type Handler = Box<dyn Fn(Call, Value) -> Result<Reply, OpError> + Send + Sync>;

/// Everything a handler may use: the context and the server's own state.
#[derive(Clone)]
pub(crate) struct Call {
    pub ctx: Arc<DaemonContext>,
    pub server: Arc<ServerState>,
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
