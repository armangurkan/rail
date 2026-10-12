use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use crossterm::event::{Event as TermEvent, KeyCode};
use futures_util::{Stream, stream};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::app::{App, Effect};
use super::handover::{Suspend, run_editor_blocking};
use super::run::run_loop;
use super::test_util::{Hello, RecordingClipboard, key};

type Log = Arc<Mutex<Vec<&'static str>>>;

/// Records the order of hand-over calls; `fail_suspend` makes the first `suspend` fail.
#[derive(Default)]
struct Recorder {
    log: Log,
    returned: bool,
    fail_suspend: bool,
}

impl Suspend for Recorder {
    fn suspend(&mut self) -> io::Result<()> {
        self.log.lock().unwrap().push("suspend");
        if self.fail_suspend {
            return Err(io::Error::other("no raw mode"));
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        self.log.lock().unwrap().push("resume");
        self.returned = true;
        Ok(())
    }

    fn take_returned(&mut self) -> bool {
        std::mem::take(&mut self.returned)
    }
}

/// A stream of keys that logs when it is dropped.
struct Logged {
    keys: stream::Iter<std::vec::IntoIter<io::Result<TermEvent>>>,
    log: Log,
}

impl Stream for Logged {
    type Item = io::Result<TermEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.keys).poll_next(cx)
    }
}

impl Drop for Logged {
    fn drop(&mut self) {
        self.log.lock().unwrap().push("drop stream");
    }
}

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(30, 3)).unwrap()
}

fn run_editor_on(program: &[&str]) -> Effect {
    Effect::RunEditor {
        command: program.iter().map(|s| (*s).to_string()).collect(),
        path: std::path::PathBuf::from("/dev/null"),
    }
}

/// A screen that runs `program` on `e`.
fn editing(program: &'static [&'static str]) -> Hello {
    Hello {
        on_key: Box::new(move |k| {
            if k.code == KeyCode::Char('e') {
                vec![run_editor_on(program)]
            } else {
                Vec::new()
            }
        }),
        ..Hello::default()
    }
}

/// Runs the loop with two streams: `e` in the first, `q` in the second.
async fn edit_then_quit(program: &'static [&'static str], recorder: &mut Recorder) -> Hello {
    let mut app = App::new(editing(program), Box::new(RecordingClipboard::default()));
    let log = recorder.log.clone();
    let mut made = 0;
    let make = move || {
        log.lock().unwrap().push("new stream");
        made += 1;
        let code = if made == 1 { 'e' } else { 'q' };
        Logged {
            keys: stream::iter(vec![Ok(TermEvent::Key(key(KeyCode::Char(code))))]),
            log: log.clone(),
        }
    };
    run_loop(&mut terminal(), &mut app, make, recorder)
        .await
        .unwrap();
    app.screen
}

#[cfg(unix)]
#[tokio::test]
async fn the_event_stream_is_dropped_before_the_editor_and_made_again_after() {
    let mut recorder = Recorder::default();
    let screen = edit_then_quit(&["/bin/sh", "-c", "exit 0"], &mut recorder).await;
    assert_eq!(screen.edited, vec![Ok(0)]);
    assert_eq!(
        *recorder.log.lock().unwrap(),
        [
            "new stream",
            "drop stream",
            "suspend",
            "resume",
            "new stream",
            "drop stream"
        ],
        "no reader of the tty exists while the editor runs"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_interrupt_for_the_editor_does_not_kill_rail() {
    // The editor signals Rail (its parent) as the terminal would on Ctrl-C.
    let mut recorder = Recorder::default();
    let program: &'static [&'static str] = &[
        "/bin/sh",
        "-c",
        "kill -INT $PPID; kill -QUIT $PPID; sleep 0.2; exit 130",
    ];
    let screen = edit_then_quit(program, &mut recorder).await;
    assert_eq!(
        screen.edited,
        vec![Ok(130)],
        "Rail lived to see the exit code"
    );
    assert_eq!(
        screen.keys.len(),
        1,
        "the loop went on to the second stream"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_editor_runs_on_a_multi_thread_runtime_too() {
    let mut recorder = Recorder::default();
    let ran = run_editor_blocking(
        &mut recorder,
        &["/bin/sh", "-c", "exit 1"].map(String::from),
        Path::new("x"),
    );
    assert_eq!(ran, Ok(1));
    assert_eq!(*recorder.log.lock().unwrap(), ["suspend", "resume"]);
}

#[test]
fn an_editor_that_cannot_start_is_reported_after_the_terminal_is_back() {
    let mut recorder = Recorder::default();
    let ran = run_editor_blocking(
        &mut recorder,
        &["/no/such/editor".to_string()],
        Path::new("x"),
    );
    assert!(ran.unwrap_err().contains("cannot run"));
    assert_eq!(*recorder.log.lock().unwrap(), ["suspend", "resume"]);
}

#[test]
fn a_failed_hand_over_still_tries_to_take_the_terminal_back() {
    let mut recorder = Recorder {
        fail_suspend: true,
        ..Recorder::default()
    };
    let ran = run_editor_blocking(
        &mut recorder,
        &["/bin/sh", "-c", "exit 0"].map(String::from),
        Path::new("x"),
    );
    let error = ran.unwrap_err();
    assert!(error.contains("cannot leave the terminal"), "{error}");
    assert_eq!(
        *recorder.log.lock().unwrap(),
        ["suspend", "resume"],
        "no editor ran"
    );
}
