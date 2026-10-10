//! The `Screen` trait every full-screen view implements: the wizard now, the mail views later.

use ratatui::Frame;

use super::app::{Effect, Msg};
use super::theme::Theme;

/// One full-screen view driven by the event loop.
pub trait Screen {
    /// React to one message and return the effects the event loop should run.
    fn update(&mut self, msg: Msg) -> Vec<Effect>;

    /// Draw the whole frame.
    fn view(&self, frame: &mut Frame, theme: &Theme);

    /// True while focus is in a text field, so `q` is typed instead of quitting.
    fn captures_text(&self) -> bool {
        false
    }

    /// True while the screen is finishing work nothing may interrupt. The quit keys then go
    /// to the screen, which quits (with `Effect::Quit`) once the work is done.
    fn defers_quit(&self) -> bool {
        false
    }
}
