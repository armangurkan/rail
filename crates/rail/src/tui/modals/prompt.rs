//! The one-line name prompt: `S` saves the filter in force under a name, `r` renames a saved
//! search. `Enter` runs `check` and refuses in place, so a clashing or malformed name never
//! reaches the daemon; `Esc` cancels.
//!
//! **Skeleton only (§12 step 0).** The type, its fields and `refusal()` exist so the
//! acceptance tests compile. Key handling and drawing are **step 9**, against the design
//! brief's slot 2: the `┌ Title ─` box, `PROMPT_WIDTH`, and the refusal wrapped through
//! `tui::text::wrap` with the box growing by the wrapped rows. Every `expect(dead_code)` below
//! fails the build the moment its item goes live, so step 9 cannot forget to remove it.

use std::fmt;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{Command, ModalOutcome};
use crate::tui::theme::Theme;
use crate::tui::widgets::text_input::TextInput;

/// What `Enter` runs before the prompt closes: `Err(reason)` refuses in place.
pub type Check = Box<dyn Fn(&str) -> Result<(), String> + Send>;

/// The prompt box's width, its border included.
#[expect(dead_code, reason = "step 0 skeleton; step 9 draws the box (refs #81)")]
pub const PROMPT_WIDTH: u16 = 56;

/// A one-line text prompt that validates before it closes.
#[expect(
    dead_code,
    reason = "step 0 skeleton; step 10 opens the prompt (refs #81)"
)]
pub struct TextPrompt {
    title: String,
    label: String,
    input: TextInput,
    /// Runs on `Enter`: `Err(reason)` keeps the box open showing `reason`.
    check: Check,
    /// What a valid name becomes.
    make: fn(String) -> Command,
    /// The last refusal, shown in place of the hint row.
    refusal: Option<String>,
}

impl fmt::Debug for TextPrompt {
    /// The title and how much was typed: a name is outside text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextPrompt")
            .field("title", &self.title)
            .field("len", &self.input.value().chars().count())
            .field("refused", &self.refusal.is_some())
            .finish()
    }
}

impl TextPrompt {
    #[expect(
        dead_code,
        reason = "step 0 skeleton; step 10 opens the prompt (refs #81)"
    )]
    pub fn new(
        title: &str,
        label: &str,
        start: &str,
        check: Check,
        make: fn(String) -> Command,
    ) -> Self {
        let mut input = TextInput::default();
        input.set_value(start);
        Self {
            title: title.to_string(),
            label: label.to_string(),
            input,
            check,
            make,
            refusal: None,
        }
    }

    /// Show `reason` in place of the hint row and keep the box open.
    #[expect(
        dead_code,
        reason = "step 0 skeleton; step 9 refuses in place (refs #81)"
    )]
    pub fn refuse(&mut self, reason: String) {
        self.refusal = Some(reason);
    }

    /// The refusal on show, if any.
    #[cfg(test)]
    pub(crate) fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    /// Step 9: edit the line, `Enter` validates through `check`, `Esc` cancels.
    pub fn key(&mut self, _key: KeyEvent) -> ModalOutcome {
        ModalOutcome::Stay
    }

    /// Step 9: the box, its label and the hint or refusal row.
    pub fn render(&self, _frame: &mut Frame, _area: Rect, _theme: &Theme) {}
}
