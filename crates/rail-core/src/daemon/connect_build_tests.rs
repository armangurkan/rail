//! Two builds of one Rail version: the connector replaces a daemon from an earlier build and
//! refuses one from a later build, and a call the daemon does not know says how to fix it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::client::DaemonClient;
use super::connect::{Connector, SpawnFn, VERSION};
use super::wire::{
    METHOD_NOT_FOUND, Message, PROTOCOL_VERSION, Response, RpcError, encode_frame, parse_message,
    read_frame,
};
use crate::error::ErrorClass;
use crate::ops::{DaemonInfoRequest, HelloResponse, names};
use crate::paths::{Paths, create_private_dir};

/// The build our connectors speak as; the fakes are earlier or later than it.
const OURS: u64 = 1_000;

/// The hello a daemon of this version sends; `build` is left out when `None`, as a daemon
/// from before the stamp does.
fn hello(paths: &Paths, build: Option<u64>, version: &str) -> Value {
    let mut hello = json!({
        "protocol": PROTOCOL_VERSION,
        "version": version,
        "pid": 1,
        "config_dir": paths.config_dir,
        "data_dir": paths.data_dir,
        "state_dir": paths.state_dir,
    });
    if let Some(build) = build {
        hello["build"] = json!(build);
    }
    hello
}

/// Listen on the socket and answer until `daemon.stop`, which removes the socket. Anything
/// but the hello and the stop is an unknown method.
async fn fake_daemon(paths: Paths, build: Option<u64>, version: String) {
    create_private_dir(&paths.runtime_dir).unwrap();
    let socket = paths.socket_file();
    let listener = UnixListener::bind(&socket).unwrap();
    let stopped = Arc::new(tokio::sync::Notify::new());
    loop {
        let (stream, _) = tokio::select! {
            accepted = listener.accept() => accepted.unwrap(),
            () = stopped.notified() => break,
        };
        let (stopped, socket, hello) = (
            stopped.clone(),
            socket.clone(),
            hello(&paths, build, &version),
        );
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            while let Ok(Some(frame)) = read_frame(&mut reader).await {
                let Ok(Message::Request(request)) = parse_message(&frame) else {
                    continue;
                };
                let response = match request.method.as_str() {
                    names::HELLO => Response::ok(request.id, hello.clone()),
                    names::DAEMON_STOP => Response::ok(request.id, json!({"stopping": true})),
                    other => Response::err(
                        Some(request.id),
                        RpcError::new(METHOD_NOT_FOUND, format!("no operation named {other:?}")),
                    ),
                };
                let _ = write.write_all(&encode_frame(&response).unwrap()).await;
                if request.method == names::DAEMON_STOP {
                    let _ = std::fs::remove_file(&socket);
                    stopped.notify_one();
                    return;
                }
            }
        });
    }
}

/// A connector of build `OURS` whose spawn starts a fake of the same build and counts starts.
fn connector(paths: &Paths) -> (Connector, Arc<AtomicUsize>) {
    let spawned = Arc::new(AtomicUsize::new(0));
    let (count, fake) = (spawned.clone(), paths.clone());
    let spawn: SpawnFn = Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(fake_daemon(fake.clone(), Some(OURS), VERSION.to_string()));
        Ok(())
    });
    (
        Connector::new(paths.clone(), spawn).with_build(OURS),
        spawned,
    )
}

/// Start a fake of `build` and wait until its socket exists.
async fn running(paths: &Paths, build: Option<u64>) {
    running_as(paths, build, VERSION).await;
}

/// `running`, announcing `version` (another spelling of this one, say).
async fn running_as(paths: &Paths, build: Option<u64>, version: &str) {
    tokio::spawn(fake_daemon(paths.clone(), build, version.to_string()));
    for _ in 0..100 {
        if paths.socket_file().exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the fake daemon never listened");
}

#[tokio::test]
async fn a_daemon_from_an_earlier_build_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running(&paths, Some(OURS - 1)).await;
    let (connector, spawned) = connector(&paths);
    let client = connector.connect().await.unwrap();
    assert_eq!(client.daemon().build, Some(OURS));
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_daemon_of_equal_version_spelled_differently_is_ordered_by_build() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running_as(&paths, Some(OURS - 1), &format!("{VERSION}+x")).await;
    let (connector, spawned) = connector(&paths);
    let client = connector.connect().await.unwrap();
    assert_eq!(client.daemon().version, VERSION);
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_daemon_that_sends_no_build_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running(&paths, None).await;
    let (connector, spawned) = connector(&paths);
    let client = connector.connect().await.unwrap();
    assert_eq!(client.daemon().build, Some(OURS));
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_daemon_from_a_later_build_is_refused_and_left_running() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running(&paths, Some(OURS + 1)).await;
    let (connector, spawned) = connector(&paths);
    let Err(err) = connector.connect().await else {
        panic!("used a daemon from a later build");
    };
    assert_eq!(err.class, ErrorClass::Permanent);
    assert_eq!(err.message, "Rail was upgraded; restart this program");
    assert_eq!(
        err.hint.as_deref(),
        Some(
            format!("the running daemon is a newer build of Rail {VERSION}; restart this program, or run `rail daemon stop` to use this build")
                .as_str()
        )
    );
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    let existing = connector.connect_existing().await.unwrap().unwrap();
    assert_eq!(
        existing.daemon().build,
        Some(OURS + 1),
        "the daemon was replaced"
    );
    connector.stop(existing).await.unwrap();
}

#[tokio::test]
async fn a_daemon_of_the_same_build_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running(&paths, Some(OURS)).await;
    let (connector, spawned) = connector(&paths);
    let client = connector.connect().await.unwrap();
    assert_eq!(client.daemon().build, Some(OURS));
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_call_the_daemon_does_not_know_says_to_stop_it() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    running(&paths, Some(OURS)).await;
    let (connector, _) = connector(&paths);
    let client: DaemonClient = connector.connect().await.unwrap();
    // The fake knows no operation but the hello and the stop.
    let Err(err) = client
        .call::<crate::ops::op::DaemonInfoOp>(&DaemonInfoRequest {})
        .await
    else {
        panic!("the fake answered an unknown method");
    };
    assert!(
        err.message.contains("no operation named"),
        "{}",
        err.message
    );
    assert_eq!(
        err.hint.as_deref(),
        Some("the daemon is older than this rail: run `rail daemon stop`")
    );
}

#[test]
fn a_hello_without_a_build_still_parses() {
    let old = json!({"protocol": 1, "version": "0.1.0", "pid": 7});
    let hello: HelloResponse = serde_json::from_value(old).unwrap();
    assert_eq!(hello.build, None);
}
