//! The event loop. Terminal events, task results and a spinner tick are polled in one
//! `select!`; each message goes through `update`, each effect is applied, and a frame is
//! drawn after every message.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::app::{App, Effect, Job, Msg, Osc52, TaggedResult, TaskResult, TaskSink, update, view};
use super::screen::Screen;
use super::terminal;

/// How often `Msg::Tick` arrives. It exists only so spinners can animate.
pub const TICK: Duration = Duration::from_millis(250);

/// The message a panicking job reports when its payload is not a string.
const PANIC_FALLBACK: &str = "task panicked";

/// Run `screen` full-screen until it quits, restore the terminal, and hand the screen back.
pub async fn run_app<S: Screen>(screen: S) -> io::Result<S> {
    let mut tui = terminal::enter()?;
    let mut app = App::new(screen, Box::new(Osc52));
    let events = Box::pin(EventStream::new());
    let result = run_loop(&mut tui.terminal, &mut app, events).await;
    drop(tui);
    result.map(|()| app.screen)
}

/// Drive `app` until it quits or `events` ends. Tasks still running when this returns are
/// aborted, because `Tasks` aborts on drop.
pub async fn run_loop<B, S, E>(
    terminal: &mut Terminal<B>,
    app: &mut App<S>,
    mut events: E,
) -> io::Result<()>
where
    B: Backend,
    S: Screen,
    E: Stream<Item = io::Result<TermEvent>> + Unpin,
{
    let (sink, mut results) = mpsc::unbounded_channel();
    let mut tasks = Tasks::new(sink);
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    draw(terminal, app)?;
    while !app.quit {
        let msg = tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => match to_msg(event) {
                    Some(msg) => msg,
                    None => continue,
                },
                Some(Err(e)) => return Err(e),
                None => return Ok(()),
            },
            Some(tagged) = results.recv() => match tasks.current(tagged) {
                Some(result) => Msg::Task(result),
                None => continue,
            },
            _ = tick.tick() => Msg::Tick,
        };
        dispatch(msg, app, &mut tasks);
        draw(terminal, app)?;
    }
    Ok(())
}

fn to_msg(event: TermEvent) -> Option<Msg> {
    match event {
        TermEvent::Key(key) if key.kind != KeyEventKind::Release => Some(Msg::Key(key)),
        TermEvent::Resize(width, height) => Some(Msg::Resize(width, height)),
        _ => None,
    }
}

/// Run `msg` through `update` and apply its effects. An effect that reports back (a copy)
/// yields a follow-up message, which is handled before the next event.
fn dispatch<S: Screen>(msg: Msg, app: &mut App<S>, tasks: &mut Tasks) {
    let mut pending = VecDeque::from([msg]);
    while let Some(msg) = pending.pop_front() {
        for effect in update(app, msg) {
            pending.extend(apply(effect, app, tasks));
        }
    }
}

fn apply<S: Screen>(effect: Effect, app: &mut App<S>, tasks: &mut Tasks) -> Option<Msg> {
    match effect {
        Effect::Quit => app.quit = true,
        Effect::CancelAll => tasks.cancel_all(),
        Effect::Spawn(job) => tasks.spawn(job),
        Effect::Copy(text) => {
            let result = app.clipboard.copy(&text).map_err(|e| e.to_string());
            if let Err(reason) = &result {
                tracing::warn!(error = %reason, "could not copy to the clipboard");
            }
            return Some(Msg::Task(TaskResult::Copied(result)));
        }
    }
    None
}

fn draw<B: Backend, S: Screen>(terminal: &mut Terminal<B>, app: &App<S>) -> io::Result<()> {
    terminal
        .draw(|frame| view(app, frame))
        .map(|_| ())
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Aborts the wrapped task when dropped, so cancelling a supervisor cancels its job too.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The tasks spawned for effects. Dropping it aborts whatever is still running, so leaving
/// the loop (quit, error, or the screen going away) cancels every effect.
pub struct Tasks {
    tx: mpsc::UnboundedSender<TaggedResult>,
    /// Bumped by every `cancel_all`; results from an older generation are stale.
    generation: u64,
    handles: Vec<JoinHandle<()>>,
}

impl Tasks {
    /// A task set whose jobs report to `tx`.
    pub fn new(tx: mpsc::UnboundedSender<TaggedResult>) -> Self {
        Self {
            tx,
            generation: 0,
            handles: Vec::new(),
        }
    }

    /// The result itself if it came from a job spawned since the last `cancel_all`, else
    /// `None`. A job aborted mid-send, or results already queued when the cancel happened,
    /// are dropped here.
    pub fn current(&self, (generation, result): TaggedResult) -> Option<TaskResult> {
        (generation == self.generation).then_some(result)
    }

    /// Start `job`. If it panics, the panic message goes to the sink as
    /// `TaskResult::JobPanicked`; if it is cancelled, nothing is sent.
    pub fn spawn(&mut self, job: Job) {
        self.handles.retain(|handle| !handle.is_finished());
        let sink = TaskSink::new(self.tx.clone(), self.generation);
        let work = AbortOnDrop(tokio::spawn({
            let sink = sink.clone();
            async move { job(sink).await }
        }));
        self.handles.push(tokio::spawn(supervise(work, sink)));
    }

    /// Abort every running job and discard anything they already sent.
    pub fn cancel_all(&mut self) {
        self.generation += 1;
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.handles.iter().filter(|h| !h.is_finished()).count()
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// Wait for a job and report a panic. Only string payloads are read; anything else gets a
/// fixed message so arbitrary data never reaches the screen.
async fn supervise(mut work: AbortOnDrop, sink: TaskSink) {
    let Err(error) = (&mut work.0).await else {
        return;
    };
    if !error.is_panic() {
        return;
    }
    let payload = error.into_panic();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| PANIC_FALLBACK.to_string());
    let _ = sink.send(TaskResult::JobPanicked(message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crossterm::event::{KeyCode, KeyEvent};
    use futures_util::stream;
    use ratatui::backend::TestBackend;
    use tokio::sync::oneshot;

    use crate::tui::app::TaskResult;
    use crate::tui::test_util::{Hello, RecordingClipboard, ctrl, key};

    fn keys(keys: Vec<KeyEvent>) -> impl Stream<Item = io::Result<TermEvent>> + Unpin {
        stream::iter(
            keys.into_iter()
                .map(|k| Ok(TermEvent::Key(k)))
                .collect::<Vec<_>>(),
        )
    }

    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A job that never finishes on its own and records when it is dropped. The returned
    /// sender keeps it waiting; drop it only after the assertion.
    fn endless_job(dropped: Arc<AtomicBool>) -> (Job, oneshot::Sender<()>) {
        let (keep_open, never) = oneshot::channel::<()>();
        let flag = SetOnDrop(dropped);
        let job: Job = Box::new(move |_sink| {
            Box::pin(async move {
                let _flag = flag;
                let _ = never.await;
            })
        });
        (job, keep_open)
    }

    async fn becomes_true(flag: &AtomicBool) -> bool {
        for _ in 0..1000 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::task::yield_now().await;
        }
        false
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(30, 3)).unwrap()
    }

    #[tokio::test]
    async fn loop_draws_the_screen_and_quits_on_q() {
        let mut terminal = terminal();
        let mut app = App::new(Hello::default(), Box::new(RecordingClipboard::default()));
        let events = keys(vec![key(KeyCode::Char('x')), key(KeyCode::Char('q'))]);
        run_loop(&mut terminal, &mut app, events).await.unwrap();
        assert!(app.quit);
        assert_eq!(app.screen.keys, vec![key(KeyCode::Char('x'))]);
        let row: String = (0..15)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert_eq!(row, "hello from rail");
    }

    #[tokio::test]
    async fn quitting_aborts_tasks_still_in_flight() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (job, _keep_open) = endless_job(dropped.clone());
        let mut job = Some(job);
        let screen = Hello {
            on_key: Box::new(move |k| {
                if k.code == KeyCode::Char('s') {
                    job.take().map(Effect::Spawn).into_iter().collect()
                } else {
                    Vec::new()
                }
            }),
            ..Hello::default()
        };
        let mut terminal = terminal();
        let mut app = App::new(screen, Box::new(RecordingClipboard::default()));
        let events = keys(vec![key(KeyCode::Char('s')), ctrl('c')]);
        run_loop(&mut terminal, &mut app, events).await.unwrap();
        assert!(
            becomes_true(&dropped).await,
            "the in-flight job was not aborted"
        );
    }

    #[tokio::test]
    async fn cancel_all_aborts_running_jobs() {
        let (sink, _results) = mpsc::unbounded_channel();
        let mut tasks = Tasks::new(sink);
        let dropped = Arc::new(AtomicBool::new(false));
        let (job, _keep_open) = endless_job(dropped.clone());
        tasks.spawn(job);
        tokio::task::yield_now().await;
        assert_eq!(tasks.in_flight(), 1);
        tasks.cancel_all();
        assert!(becomes_true(&dropped).await);
        assert_eq!(tasks.in_flight(), 0);
    }

    #[tokio::test]
    async fn cancelled_jobs_report_nothing() {
        let (sink, mut results) = mpsc::unbounded_channel();
        let mut tasks = Tasks::new(sink);
        let dropped = Arc::new(AtomicBool::new(false));
        let (job, _keep_open) = endless_job(dropped.clone());
        tasks.spawn(job);
        tokio::task::yield_now().await;
        tasks.cancel_all();
        assert!(becomes_true(&dropped).await);
        drop(tasks);
        assert!(
            results.recv().await.is_none(),
            "a cancelled job sent a result"
        );
    }

    #[tokio::test]
    async fn results_sent_before_cancel_all_are_dropped() {
        let (tx, mut results) = mpsc::unbounded_channel();
        let mut tasks = Tasks::new(tx);
        let (sent_tx, sent_rx) = oneshot::channel();
        let (_keep_open, never) = oneshot::channel::<()>();
        tasks.spawn(Box::new(move |sink| {
            Box::pin(async move {
                let _ = sink.send(TaskResult::Copied(Ok(())));
                let _ = sent_tx.send(());
                let _ = never.await;
            })
        }));
        sent_rx.await.unwrap();
        tasks.cancel_all();
        tasks.spawn(Box::new(|sink| {
            Box::pin(async move {
                let _ = sink.send(TaskResult::Copied(Err("fresh".into())));
            })
        }));
        let mut delivered = Vec::new();
        for _ in 0..2 {
            let tagged = results.recv().await.expect("both results were queued");
            delivered.extend(tasks.current(tagged));
        }
        match delivered.as_slice() {
            [TaskResult::Copied(Err(reason))] => assert_eq!(reason, "fresh"),
            other => panic!("expected only the post-cancel result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_panicking_job_is_reported_with_its_message() {
        let (sink, mut results) = mpsc::unbounded_channel();
        let mut tasks = Tasks::new(sink);
        tasks.spawn(Box::new(|_sink| Box::pin(async { panic!("boom") })));
        match results.recv().await {
            Some((_, TaskResult::JobPanicked(message))) => assert_eq!(message, "boom"),
            other => panic!("expected JobPanicked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn copy_effects_go_to_the_clipboard_and_report_back() {
        let clipboard = RecordingClipboard::default();
        let screen = Hello {
            on_key: Box::new(|k| {
                if k.code == KeyCode::Char('y') {
                    vec![Effect::Copy("consent url".into())]
                } else {
                    Vec::new()
                }
            }),
            ..Hello::default()
        };
        let mut terminal = terminal();
        let mut app = App::new(screen, Box::new(clipboard.clone()));
        let events = keys(vec![key(KeyCode::Char('y')), key(KeyCode::Char('q'))]);
        run_loop(&mut terminal, &mut app, events).await.unwrap();
        assert_eq!(
            *clipboard.0.lock().unwrap(),
            vec!["consent url".to_string()]
        );
        assert_eq!(app.screen.copied, vec![Ok(())]);
    }
}
