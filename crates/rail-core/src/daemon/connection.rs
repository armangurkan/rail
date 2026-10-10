//! One client connection: the hello first, then each request on its own task so a long
//! `pending.wait` never holds up the others. When the client goes away its in-flight requests
//! are aborted; work they started on the blocking pool or in detached tasks carries on.

use std::ops::ControlFlow;
use std::sync::Arc;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::context::DaemonContext;
use super::registry::{Call, Registry};
use super::server::ServerState;
use super::wire::{
    INVALID_PARAMS, INVALID_REQUEST, Message, PARSE_ERROR, Response, RpcError, WireError,
    encode_frame, parse_message, read_frame,
};
use crate::ops::{HelloRequest, OpError, names};

pub(crate) struct Connection {
    pub ctx: Arc<DaemonContext>,
    pub registry: Arc<Registry>,
    pub server: Arc<ServerState>,
}

impl Connection {
    pub(crate) async fn serve(self, stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let (frames, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if write.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = write.shutdown().await;
        });
        let mut tasks = JoinSet::new();
        self.read_requests(read, &frames, &mut tasks).await;
        tasks.abort_all();
        self.server.client_disconnected();
    }

    async fn read_requests(
        &self,
        read: OwnedReadHalf,
        frames: &mpsc::UnboundedSender<Vec<u8>>,
        tasks: &mut JoinSet<()>,
    ) {
        let mut reader = BufReader::new(read);
        let mut greeted = false;
        loop {
            let next = if greeted {
                read_frame(&mut reader).await
            } else {
                // A client that never says hello must not keep the daemon from going idle.
                let wait = self.server.config.hello_timeout;
                tokio::time::timeout(wait, read_frame(&mut reader))
                    .await
                    .unwrap_or(Ok(None))
            };
            let frame = match next {
                Ok(Some(frame)) => frame,
                Ok(None) => return,
                Err(WireError::TooLarge) => {
                    let error = RpcError::new(INVALID_REQUEST, "a message is larger than 1 MiB");
                    reply(frames, Response::err(None, error));
                    return;
                }
                Err(_) => return,
            };
            if self.handle(&frame, &mut greeted, frames, tasks).is_break() {
                return;
            }
        }
    }

    fn handle(
        &self,
        frame: &[u8],
        greeted: &mut bool,
        frames: &mpsc::UnboundedSender<Vec<u8>>,
        tasks: &mut JoinSet<()>,
    ) -> ControlFlow<()> {
        let request = match parse_message(frame) {
            Ok(Message::Request(request)) => request,
            Ok(_) => return ControlFlow::Continue(()),
            Err(e) => {
                reply(
                    frames,
                    Response::err(None, RpcError::new(PARSE_ERROR, e.to_string())),
                );
                return ControlFlow::Continue(());
            }
        };
        if !*greeted {
            if let Some(error) = hello_problem(&request) {
                reply(frames, Response::err(Some(request.id), error));
                return ControlFlow::Break(());
            }
            *greeted = true;
        }
        self.server.touch();
        let call = Call {
            ctx: self.ctx.clone(),
            server: self.server.clone(),
        };
        let (registry, frames) = (self.registry.clone(), frames.clone());
        // A finished task is freed only when joined: drain them, or a long-lived connection
        // grows with every request it ever made.
        while let Some(joined) = tasks.try_join_next() {
            if let Err(e) = joined
                && e.is_panic()
            {
                tracing::warn!("a request handler panicked");
            }
        }
        tasks.spawn(async move {
            let response = registry.dispatch(call, request).await;
            reply(&frames, response);
        });
        #[cfg(test)]
        self.server.note_tasks(tasks.len());
        ControlFlow::Continue(())
    }
}

/// Why `request` cannot be the first one on a connection: not a hello, or a hello whose
/// parameters do not parse.
fn hello_problem(request: &super::wire::Request) -> Option<RpcError> {
    if request.method != names::HELLO {
        return Some(RpcError::new(
            INVALID_REQUEST,
            "the first request must be hello",
        ));
    }
    let params = match &request.params {
        serde_json::Value::Null => serde_json::json!({}),
        params => params.clone(),
    };
    match serde_json::from_value::<HelloRequest>(params) {
        Ok(_) => None,
        Err(e) => {
            let mut rpc = RpcError::from_op(&OpError::invalid_params(&e));
            rpc.code = INVALID_PARAMS;
            Some(rpc)
        }
    }
}

fn reply(frames: &mpsc::UnboundedSender<Vec<u8>>, response: Response) {
    match encode_frame(&response) {
        Ok(frame) => {
            let _ = frames.send(frame);
        }
        Err(e) => tracing::error!(error = %e, "could not encode a response"),
    }
}
