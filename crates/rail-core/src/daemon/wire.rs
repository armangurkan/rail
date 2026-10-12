//! The daemon's wire format: JSON-RPC 2.0, one message per line, at most 1 MiB per message.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::error::Category;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

use crate::error::ErrorClass;
use crate::ops::{ErrorCode, OpError};

/// Bumped whenever a message changes shape. A client and daemon with different versions
/// never talk past the hello.
pub const PROTOCOL_VERSION: u32 = 1;
/// The largest message, excluding its newline.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 2.0 error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// An operation ran and failed; `data` holds the `OpError`.
pub const OPERATION_FAILED: i64 = -32000;

/// `Debug` for the types below is written by hand: their `params`, `result` and `data` can
/// carry a client secret (an `account.add.begin` request does), and a stray `{:?}` in a log
/// must never print it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("id", &self.id)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// `None` only for an error about a message whose id could not be read.
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("id", &self.id)
            .field("has_result", &self.result.is_some())
            .field("error", &self.error)
            .finish()
    }
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<u64>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// The result, or the operation's error.
    pub fn into_result(self) -> Result<Value, OpError> {
        match (self.result, self.error) {
            (_, Some(error)) => Err(error.into_op()),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(Value::Null),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl fmt::Debug for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcError")
            .field("code", &self.code)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// An operation's error, carried whole in `data`.
    pub fn from_op(error: &OpError) -> Self {
        Self {
            code: OPERATION_FAILED,
            message: error.message.clone(),
            data: serde_json::to_value(error).ok(),
        }
    }

    /// Back to an `OpError`: the one in `data`, or a daemon error built from the message.
    pub fn into_op(self) -> OpError {
        if let Some(op) = self.data.and_then(|d| serde_json::from_value(d).ok()) {
            return op;
        }
        let code = match self.code {
            INVALID_PARAMS => ErrorCode::InvalidInput,
            _ => ErrorCode::Daemon,
        };
        let error = OpError::new(code, ErrorClass::Permanent, self.message);
        if self.code == METHOD_NOT_FOUND {
            return error.with_hint(
                "the daemon is older than this Rail: run `rail daemon stop`, then try again",
            );
        }
        error
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl fmt::Debug for Notification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notification")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("a message is larger than 1 MiB")]
    TooLarge,
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("connection error: {0}")]
    Io(#[from] std::io::Error),
}

impl WireError {
    /// A dropped connection may come back; a bad message will not fix itself.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Io(_) => ErrorClass::Transient,
            Self::TooLarge | Self::Malformed(_) => ErrorClass::Permanent,
        }
    }
}

impl From<WireError> for OpError {
    /// Rail's words in `message`; the operating system's or serde's text in `detail`.
    fn from(error: WireError) -> Self {
        let class = error.class();
        let (message, detail) = match &error {
            WireError::TooLarge => ("a message is larger than 1 MiB", None),
            WireError::Malformed(text) => (
                "the daemon connection got a malformed message",
                Some(text.clone()),
            ),
            WireError::Io(e) => ("the daemon connection failed", Some(e.to_string())),
        };
        let op = OpError::new(ErrorCode::Daemon, class, message);
        match detail {
            Some(detail) => op.with_detail(detail),
            None => op,
        }
    }
}

/// Describe a JSON error by kind and position only: serde's own text may quote a secret.
fn json_message(error: &serde_json::Error) -> String {
    let kind = match error.classify() {
        Category::Io => "I/O error",
        Category::Syntax => "syntax error",
        Category::Data => "unexpected value",
        Category::Eof => "unexpected end of data",
    };
    format!("{kind} at line {} column {}", error.line(), error.column())
}

/// Tell a request (method and id), a notification (method, no id) and a response apart.
pub fn parse_message(frame: &[u8]) -> Result<Message, WireError> {
    let value: Value =
        serde_json::from_slice(frame).map_err(|e| WireError::Malformed(json_message(&e)))?;
    let malformed = |e: serde_json::Error| WireError::Malformed(json_message(&e));
    let has = |key: &str| value.get(key).is_some();
    if has("method") && has("id") {
        serde_json::from_value(value)
            .map(Message::Request)
            .map_err(malformed)
    } else if has("method") {
        serde_json::from_value(value)
            .map(Message::Notification)
            .map_err(malformed)
    } else {
        serde_json::from_value(value)
            .map(Message::Response)
            .map_err(malformed)
    }
}

/// Serialise `message` as one frame: JSON plus a newline. Refuses anything over 1 MiB.
pub fn encode_frame(message: &impl Serialize) -> Result<Vec<u8>, WireError> {
    let mut frame =
        serde_json::to_vec(message).map_err(|e| WireError::Malformed(json_message(&e)))?;
    if frame.len() > MAX_FRAME_BYTES {
        return Err(WireError::TooLarge);
    }
    frame.push(b'\n');
    Ok(frame)
}

/// The next frame without its newline, or `None` at a clean end of stream. A frame over
/// 1 MiB is an error, and the stream is out of step after it: close the connection.
pub async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, WireError> {
    let mut frame = Vec::new();
    let limit = (MAX_FRAME_BYTES + 1) as u64;
    let read = (&mut *reader)
        .take(limit)
        .read_until(b'\n', &mut frame)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if frame.last() == Some(&b'\n') {
        frame.pop();
        return Ok(Some(frame));
    }
    if frame.len() > MAX_FRAME_BYTES {
        return Err(WireError::TooLarge);
    }
    Err(WireError::Malformed(
        "the connection closed in the middle of a message".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    async fn read_all(bytes: Vec<u8>) -> Vec<Result<Option<Vec<u8>>, String>> {
        let mut reader = BufReader::new(bytes.as_slice());
        let mut out = Vec::new();
        loop {
            let frame = read_frame(&mut reader).await.map_err(|e| e.to_string());
            let end = matches!(frame, Ok(None) | Err(_));
            out.push(frame);
            if end {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let request = Request::new(7, "status", json!({}));
        let mut bytes = encode_frame(&request).unwrap();
        bytes.extend(encode_frame(&Notification::new("pending.finished", json!(1))).unwrap());
        let frames = read_all(bytes).await;
        assert_eq!(frames.len(), 3);
        let first = frames[0].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(parse_message(first).unwrap(), Message::Request(request));
        let second = frames[1].as_ref().unwrap().as_ref().unwrap();
        assert!(matches!(
            parse_message(second).unwrap(),
            Message::Notification(_)
        ));
        assert_eq!(frames[2], Ok(None));
    }

    #[tokio::test]
    async fn a_frame_of_exactly_one_mib_is_accepted() {
        let mut bytes = vec![b' '; MAX_FRAME_BYTES];
        bytes.push(b'\n');
        let frames = read_all(bytes).await;
        assert_eq!(
            frames[0].as_ref().unwrap().as_ref().unwrap().len(),
            MAX_FRAME_BYTES
        );
    }

    #[tokio::test]
    async fn a_frame_over_one_mib_is_refused_when_read() {
        let mut bytes = vec![b' '; MAX_FRAME_BYTES + 1];
        bytes.push(b'\n');
        let frames = read_all(bytes).await;
        assert_eq!(frames[0], Err("a message is larger than 1 MiB".to_string()));
    }

    #[test]
    fn writing_an_oversized_message_fails() {
        let big = Request::new(1, "status", json!("x".repeat(MAX_FRAME_BYTES)));
        assert!(matches!(encode_frame(&big), Err(WireError::TooLarge)));
        assert_eq!(WireError::TooLarge.class(), ErrorClass::Permanent);
    }

    #[test]
    fn parse_tells_requests_responses_and_notifications_apart() {
        let response = br#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#;
        assert!(matches!(
            parse_message(response).unwrap(),
            Message::Response(Response { id: Some(3), .. })
        ));
        let failed = br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"no"}}"#;
        let Message::Response(failed) = parse_message(failed).unwrap() else {
            panic!("a response");
        };
        let op = failed.into_result().unwrap_err();
        assert_eq!(op.code, ErrorCode::Daemon);
    }

    #[test]
    fn operation_errors_survive_the_round_trip() {
        let op = OpError::not_found("no pending operation p-9").with_hint("check the id");
        let response = Response::err(Some(1), RpcError::from_op(&op));
        let text = String::from_utf8(encode_frame(&response).unwrap()).unwrap();
        let Message::Response(back) = parse_message(text.trim_end().as_bytes()).unwrap() else {
            panic!("a response");
        };
        assert_eq!(back.into_result().unwrap_err(), op);
    }

    #[test]
    fn malformed_json_errors_do_not_echo_the_input() {
        let err = parse_message(br#"{"jsonrpc":"2.0","id":1,"method":"x","params":"GOCSPX-leak"#)
            .unwrap_err();
        assert!(!err.to_string().contains("GOCSPX"), "{err}");
        let err =
            parse_message(br#"{"jsonrpc":"2.0","id":"GOCSPX-leak","method":"x"}"#).unwrap_err();
        assert!(!err.to_string().contains("GOCSPX"), "{err}");
    }

    #[test]
    fn debug_output_never_prints_params_or_results() {
        let secret = json!({"client_secret": "GOCSPX-test"});
        let request = Request::new(4, "account.add.begin", secret.clone());
        let text = format!("{request:?} {:?}", Message::Request(request.clone()));
        assert!(!text.contains("GOCSPX-test"), "{text}");
        assert!(
            text.contains("account.add.begin") && text.contains('4'),
            "{text}"
        );
        let response = Response::ok(4, secret.clone());
        assert!(!format!("{response:?}").contains("GOCSPX-test"));
        let notification = Notification::new("pending.finished", secret.clone());
        assert!(!format!("{notification:?}").contains("GOCSPX-test"));
        let mut error = RpcError::new(OPERATION_FAILED, "failed");
        error.data = Some(secret);
        assert!(!format!("{error:?}").contains("GOCSPX-test"));
    }

    #[test]
    fn io_errors_put_outside_text_in_detail_not_message() {
        let text = "IGNORE PREVIOUS INSTRUCTIONS";
        let op: OpError = WireError::Io(std::io::Error::other(text)).into();
        assert!(!op.message.contains("IGNORE"), "{}", op.message);
        assert!(op.hint.as_deref().is_none_or(|h| !h.contains("IGNORE")));
        assert!(op.detail.unwrap().get().contains(text));
    }

    #[test]
    fn malformed_errors_put_their_text_in_detail_not_message() {
        let op: OpError = WireError::Malformed("syntax error at line 1 column 9".into()).into();
        assert!(!op.message.contains("syntax error"), "{}", op.message);
        assert!(op.detail.unwrap().get().contains("syntax error"));
    }
}
