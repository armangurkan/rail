//! One client connection: the hello first, then each request on its own task so a long
//! `pending.wait` never holds up the others. When the client goes away its in-flight requests
//! are aborted; work they started on the blocking pool or in detached tasks carries on.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::context::DaemonContext;
use super::registry::{Call, Caller, Registry, Watch};
use super::server::ServerState;
use super::wire::{
    INVALID_PARAMS, INVALID_REQUEST, Message, PARSE_ERROR, Response, RpcError, WireError,
    encode_frame, parse_message, read_frame,
};
use crate::ops::{BrowserEnv, HelloRequest, OpError, names};

/// How long a closing connection lets its running requests answer.
const REQUEST_DRAIN: Duration = Duration::from_secs(1);

/// How long a closing connection lets its writer flush queued frames.
const WRITER_DRAIN: Duration = Duration::from_secs(1);

/// Aborts the writer task when dropped while still armed, that is, when the connection task is
/// aborted at shutdown. A normal end disarms it so queued frames are still flushed.
struct AbortWriter(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortWriter {
    fn drop(&mut self) {
        if let Some(writer) = self.0.take() {
            writer.abort();
        }
    }
}

pub(crate) struct Connection {
    pub ctx: Arc<DaemonContext>,
    pub registry: Arc<Registry>,
    pub server: Arc<ServerState>,
}

impl Connection {
    pub(crate) async fn serve(self, stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let (frames, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if write.write_all(&frame).await.is_err() {
                    break;
                }
            }
            if let Err(e) = write.shutdown().await {
                tracing::debug!(error = %e, "closing a connection");
            }
        });
        // If the daemon ends this task, the socket must close with it.
        let mut guard = AbortWriter(Some(writer));
        let mut tasks = JoinSet::new();
        self.read_requests(read, &frames, &mut tasks).await;
        if self.server.is_closing() {
            // Shutdown: let requests already running answer before the writer is drained.
            let _ = tokio::time::timeout(REQUEST_DRAIN, async {
                while tasks.join_next().await.is_some() {}
            })
            .await;
        }
        tasks.abort_all();
        self.server.client_disconnected();
        drop(frames);
        // The writer ends once its queue is empty and flushed; a client that never reads
        // cannot hold it past the bound.
        if let Some(mut writer) = guard.0.take()
            && tokio::time::timeout(WRITER_DRAIN, &mut writer)
                .await
                .is_err()
        {
            writer.abort();
        }
    }

    async fn read_requests(
        &self,
        read: OwnedReadHalf,
        frames: &mpsc::UnboundedSender<Vec<u8>>,
        tasks: &mut JoinSet<()>,
    ) {
        let mut reader = BufReader::new(read);
        let mut hello: Option<BrowserEnv> = None;
        let watching = Arc::new(Watch::default());
        loop {
            let next = tokio::select! {
                next = self.next_frame(&mut reader, hello.is_some()) => next,
                () = self.server.closed() => return,
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
            if self
                .handle(&frame, &mut hello, &watching, frames, tasks)
                .is_break()
            {
                return;
            }
        }
    }

    async fn next_frame(
        &self,
        reader: &mut BufReader<OwnedReadHalf>,
        greeted: bool,
    ) -> Result<Option<Vec<u8>>, WireError> {
        if greeted {
            return read_frame(reader).await;
        }
        // A client that never says hello must not keep the daemon from going idle.
        let wait = self.server.config.hello_timeout;
        tokio::time::timeout(wait, read_frame(reader))
            .await
            .unwrap_or(Ok(None))
    }

    fn handle(
        &self,
        frame: &[u8],
        hello: &mut Option<BrowserEnv>,
        watching: &Arc<Watch>,
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
        if hello.is_none() {
            match read_hello(&request) {
                Ok(first) => *hello = Some(first.browser.checked()),
                Err(error) => {
                    reply(frames, Response::err(Some(request.id), error));
                    return ControlFlow::Break(());
                }
            }
        }
        self.server.touch();
        let call = Call {
            ctx: self.ctx.clone(),
            server: self.server.clone(),
            caller: Caller::new(frames.clone(), hello.clone().unwrap_or_default())
                .sharing_watch(watching.clone()),
        };
        let (registry, frames) = (self.registry.clone(), frames.clone());
        reap_finished(tasks);
        tasks.spawn(async move {
            let response = registry.dispatch(call, request).await;
            reply(&frames, response);
        });
        #[cfg(test)]
        self.server.note_tasks(tasks.len());
        ControlFlow::Continue(())
    }
}

/// A finished task is freed only when joined: drain them, or a long-lived connection grows
/// with every request it ever made.
fn reap_finished(tasks: &mut JoinSet<()>) {
    while let Some(joined) = tasks.try_join_next() {
        if let Err(e) = joined
            && e.is_panic()
        {
            tracing::warn!("a request handler panicked");
        }
    }
}

/// The first request on a connection, which must be a hello whose parameters parse.
fn read_hello(request: &super::wire::Request) -> Result<HelloRequest, RpcError> {
    if request.method != names::HELLO {
        return Err(RpcError::new(
            INVALID_REQUEST,
            "the first request must be hello",
        ));
    }
    let params = match &request.params {
        serde_json::Value::Null => serde_json::json!({}),
        params => params.clone(),
    };
    serde_json::from_value::<HelloRequest>(params).map_err(|e| {
        let mut rpc = RpcError::from_op(&OpError::invalid_params(&e));
        rpc.code = INVALID_PARAMS;
        rpc
    })
}

/// Queue `response`. One that cannot be sent (over the frame limit, or not encodable) is
/// replaced by an `INTERNAL` error for the same id, so the caller is never left waiting.
fn reply(frames: &mpsc::UnboundedSender<Vec<u8>>, response: Response) {
    let id = response.id;
    let frame = encode_frame(&response).or_else(|e| {
        tracing::error!(error = %e, "could not send a response; sending an error instead");
        let message = match e {
            WireError::TooLarge => "the answer was too large to send",
            _ => "the answer could not be encoded",
        };
        encode_frame(&Response::err(
            id,
            RpcError::from_op(&OpError::internal(message)),
        ))
    });
    match frame {
        Ok(frame) => {
            let _ = frames.send(frame);
        }
        Err(e) => tracing::error!(error = %e, "could not encode an error response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::wire::MAX_FRAME_BYTES;
    use crate::ops::ErrorCode;

    /// Final review M6: an answer over the frame limit becomes an INTERNAL error frame for the
    /// same id, so the caller gets an answer instead of waiting forever.
    #[test]
    fn an_answer_too_large_to_send_becomes_an_internal_error() {
        let (frames, mut sent) = mpsc::unbounded_channel();
        let huge = serde_json::Value::String("x".repeat(MAX_FRAME_BYTES + 1));
        reply(&frames, Response::ok(7, huge));
        let frame = sent.try_recv().expect("an answer was sent");
        assert!(frame.len() < 4096, "the error frame is small");
        let Ok(Message::Response(answer)) = parse_message(&frame[..frame.len() - 1]) else {
            panic!("not a response: {}", String::from_utf8_lossy(&frame));
        };
        assert_eq!(answer.id, Some(7));
        assert!(answer.result.is_none());
        let error: OpError = serde_json::from_value(answer.error.unwrap().data.unwrap()).unwrap();
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.message, "the answer was too large to send");
    }

    #[test]
    fn an_answer_within_the_limit_is_sent_as_is() {
        let (frames, mut sent) = mpsc::unbounded_channel();
        reply(&frames, Response::ok(3, serde_json::json!({"fine": true})));
        let frame = sent.try_recv().unwrap();
        let Ok(Message::Response(answer)) = parse_message(&frame[..frame.len() - 1]) else {
            panic!("not a response");
        };
        assert_eq!(answer.result, Some(serde_json::json!({"fine": true})));
    }
}
