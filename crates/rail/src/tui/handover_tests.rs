use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use crossterm::event::{Event as TermEvent, KeyCode};
use futures_util::{Stream, stream};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

use super::app::{App, Effect};
use super::handover::{Suspend, run_editor_blocking};
use super::run::run_loop;
use super::test_util::{Hello, RecordingClipboard, key};

type Log = Arc<Mutex<Vec<&'static str>>>;

/// Records the order of hand-over calls; `fail_suspend` makes the first `suspend` fail,
/// `fail_resume` every `resume`.
#[derive(Default)]
struct Recorder {
    log: Log,
    returned: bool,
    fail_suspend: bool,
    fail_resume: bool,
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
        if self.fail_resume {
            return Err(io::Error::other("no raw mode"));
        }
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

/// `TestBackend`'s errors cannot happen.
fn never(error: std::convert::Infallible) -> io::Error {
    match error {}
}

/// A `TestBackend` that cannot report the cursor position, as a terminal cannot while the
/// event stream's reader waits for input, and that counts the full-screen clears.
struct NoCursor {
    inner: TestBackend,
    full_clears: Arc<AtomicUsize>,
}

impl Backend for NoCursor {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content).map_err(never)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor().map_err(never)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor().map_err(never)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Err(io::Error::other(
            "The cursor position could not be read within a normal duration",
        ))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position).map_err(never)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear().map_err(never)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        if clear_type == ClearType::All {
            self.full_clears.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.clear_region(clear_type).map_err(never)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size().map_err(never)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size().map_err(never)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().map_err(never)
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_editor_round_does_not_ask_for_the_cursor() {
    let full_clears = Arc::new(AtomicUsize::new(0));
    let mut terminal = Terminal::new(NoCursor {
        inner: TestBackend::new(30, 3),
        full_clears: full_clears.clone(),
    })
    .unwrap();
    let mut app = App::new(
        editing(&["/bin/sh", "-c", "exit 0"]),
        Box::new(RecordingClipboard::default()),
    );
    let mut made = 0;
    let make = move || {
        made += 1;
        let code = if made == 1 { 'e' } else { 'q' };
        stream::iter(vec![Ok(TermEvent::Key(key(KeyCode::Char(code))))])
    };
    let mut recorder = Recorder::default();

    run_loop(&mut terminal, &mut app, make, &mut recorder)
        .await
        .expect("the editor round must not need the cursor position");

    assert_eq!(app.screen.edited, vec![Ok(0)]);
    assert_eq!(
        full_clears.load(Ordering::SeqCst),
        1,
        "one repaint, after the editor"
    );
    let row: String = (0..15)
        .map(|x| terminal.backend().inner.buffer()[(x, 0)].symbol())
        .collect();
    assert_eq!(
        row, "hello from rail",
        "the clear wiped the screen and the frame refilled it"
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

/// Final fix 7: once the editor ran, its exit code is the answer even when taking the
/// terminal back fails (the screen repaints anyway), so the text it wrote is read.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_resume_after_the_editor_ran_still_answers_its_exit_code() {
    let mut recorder = Recorder {
        fail_resume: true,
        ..Recorder::default()
    };
    let screen = edit_then_quit(&["/bin/sh", "-c", "exit 0"], &mut recorder).await;
    assert_eq!(screen.edited, vec![Ok(0)]);
}
