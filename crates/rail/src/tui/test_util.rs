//! Helpers shared by the TUI unit tests.

use std::io;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};

use super::app::{Clipboard, Effect, Msg, TaskResult};
use super::screen::Screen;
use super::theme::Theme;

/// The two sizes every rendering test checks: a classic terminal and a roomy one.
pub const SIZES: [(u16, u16); 2] = [(80, 24), (120, 40)];

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Draw with `draw` on a `width` x `height` test terminal and return the visible rows.
pub fn render_lines(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(draw).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// Records copies instead of touching the real clipboard.
#[derive(Clone, Default)]
pub struct RecordingClipboard(pub Arc<Mutex<Vec<String>>>);

impl Clipboard for RecordingClipboard {
    fn copy(&mut self, text: &str) -> io::Result<()> {
        self.0
            .lock()
            .expect("clipboard lock")
            .push(text.to_string());
        Ok(())
    }
}

/// A screen that says hello, remembers the keys and copy reports it receives, and answers
/// keys with `on_key`.
pub struct Hello {
    pub typing: bool,
    pub keys: Vec<KeyEvent>,
    /// Every `TaskResult::Copied` the screen was told about.
    pub copied: Vec<Result<(), String>>,
    pub on_key: Box<dyn FnMut(KeyEvent) -> Vec<Effect>>,
}

impl Default for Hello {
    fn default() -> Self {
        Self {
            typing: false,
            keys: Vec::new(),
            copied: Vec::new(),
            on_key: Box::new(|_| Vec::new()),
        }
    }
}

impl Screen for Hello {
    fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                self.keys.push(key);
                (self.on_key)(key)
            }
            Msg::Task(TaskResult::Copied(result)) => {
                self.copied.push(result);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn view(&self, frame: &mut Frame, theme: &Theme) {
        frame.render_widget(
            Paragraph::new("hello from rail").style(theme.text),
            frame.area(),
        );
    }

    fn captures_text(&self) -> bool {
        self.typing
    }
}
