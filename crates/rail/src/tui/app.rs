//! The shell's model (the active screen, the theme and the quit flag), the messages that
//! drive it, and the effects it asks the event loop to run.

use std::io;

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use futures_util::future::BoxFuture;
use ratatui::Frame;
use tokio::sync::mpsc;

use super::screen::Screen;
use super::theme::Theme;

/// Everything the shell reacts to.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Resize(u16, u16),
    /// Every 250 ms, so spinners can animate.
    Tick,
    Task(TaskResult),
}

/// What a background task, or the event loop on its behalf, reports back to the screen.
#[derive(Debug)]
pub enum TaskResult {
    /// A spawned job panicked; carries the panic message when it was a string.
    JobPanicked(String),
    /// The outcome of an `Effect::Copy`: `Err` holds the reason it could not copy.
    Copied(Result<(), String>),
}

/// Where a running task sends its results.
pub type TaskSink = mpsc::UnboundedSender<TaskResult>;

/// A unit of background work. The event loop starts it and aborts it when it is cancelled.
pub type Job = Box<dyn FnOnce(TaskSink) -> BoxFuture<'static, ()> + Send>;

/// What `update` asks the event loop to do.
pub enum Effect {
    Quit,
    /// Abort every task still running.
    CancelAll,
    Spawn(Job),
    /// Put text on the system clipboard, then report `TaskResult::Copied`.
    Copy(String),
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quit => f.write_str("Quit"),
            Self::CancelAll => f.write_str("CancelAll"),
            Self::Spawn(_) => f.write_str("Spawn(..)"),
            Self::Copy(_) => f.write_str("Copy(..)"),
        }
    }
}

/// The system clipboard, behind a trait so tests can record instead.
pub trait Clipboard {
    /// Put `text` on the clipboard.
    fn copy(&mut self, text: &str) -> io::Result<()>;
}

/// Copies through the terminal with an OSC 52 sequence, which also works over SSH. The event
/// loop writes it between frames; it is a control sequence, never text on the screen.
pub struct Osc52;

impl Clipboard for Osc52 {
    fn copy(&mut self, text: &str) -> io::Result<()> {
        execute!(io::stdout(), CopyToClipboard::to_clipboard_from(text))
    }
}

/// The shell's state: the active screen plus what is shared across screens.
pub struct App<S> {
    pub screen: S,
    pub theme: Theme,
    pub quit: bool,
    pub clipboard: Box<dyn Clipboard>,
}

impl<S: Screen> App<S> {
    /// A shell showing `screen`, with the default theme.
    pub fn new(screen: S, clipboard: Box<dyn Clipboard>) -> Self {
        Self {
            screen,
            theme: Theme::default(),
            quit: false,
            clipboard,
        }
    }
}

/// `Ctrl-C` always quits; `q` quits unless the screen is taking text. Everything else goes
/// to the screen.
pub fn update<S: Screen>(app: &mut App<S>, msg: Msg) -> Vec<Effect> {
    if let Msg::Key(key) = &msg
        && quits(key, app.screen.captures_text())
    {
        app.quit = true;
        return vec![Effect::Quit];
    }
    if let Msg::Resize(width, height) = &msg {
        tracing::trace!(width, height, "terminal resized");
    }
    app.screen.update(msg)
}

fn quits(key: &KeyEvent, typing: bool) -> bool {
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    let q = key.code == KeyCode::Char('q') && key.modifiers.is_empty() && !typing;
    ctrl_c || q
}

/// Draw the active screen.
pub fn view<S: Screen>(app: &App<S>, frame: &mut Frame) {
    app.screen.view(frame, &app.theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{Hello, RecordingClipboard, SIZES, ctrl, key, render_lines};

    fn app(typing: bool) -> App<Hello> {
        let screen = Hello {
            typing,
            ..Hello::default()
        };
        App::new(screen, Box::new(RecordingClipboard::default()))
    }

    #[test]
    fn ctrl_c_quits_even_while_typing() {
        let mut app = app(true);
        let effects = update(&mut app, Msg::Key(ctrl('c')));
        assert!(app.quit);
        assert!(matches!(effects.as_slice(), [Effect::Quit]), "{effects:?}");
        assert!(
            app.screen.keys.is_empty(),
            "the screen never sees the quit key"
        );
    }

    #[test]
    fn q_quits_outside_a_text_field() {
        let mut app = app(false);
        let effects = update(&mut app, Msg::Key(key(KeyCode::Char('q'))));
        assert!(app.quit);
        assert!(matches!(effects.as_slice(), [Effect::Quit]), "{effects:?}");
    }

    #[test]
    fn q_is_typed_inside_a_text_field() {
        let mut app = app(true);
        let effects = update(&mut app, Msg::Key(key(KeyCode::Char('q'))));
        assert!(!app.quit);
        assert!(effects.is_empty());
        assert_eq!(app.screen.keys, vec![key(KeyCode::Char('q'))]);
    }

    #[test]
    fn other_messages_reach_the_screen() {
        let mut app = app(false);
        update(&mut app, Msg::Key(key(KeyCode::Char('x'))));
        assert!(update(&mut app, Msg::Tick).is_empty());
        assert!(update(&mut app, Msg::Resize(100, 30)).is_empty());
        assert_eq!(app.screen.keys.len(), 1);
        assert!(!app.quit);
    }

    #[test]
    fn view_draws_the_active_screen_at_both_sizes() {
        let app = app(false);
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| view(&app, frame));
            assert!(lines[0].starts_with("hello from rail"), "{lines:?}");
        }
    }
}
