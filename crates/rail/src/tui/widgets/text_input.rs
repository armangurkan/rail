//! A single-line text input with a cursor and a masked mode for secrets, and the labelled
//! field that draws it.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::theme::Theme;

/// Columns reserved for the label in front of every field.
pub const LABEL_WIDTH: u16 = 15;
/// Rows one field takes: the value, then its error or hint.
pub const FIELD_HEIGHT: u16 = 2;

/// Editable single-line text. The value may be a secret, so `Debug` never prints it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    value: String,
    /// In characters, not bytes.
    cursor: usize,
    masked: bool,
}

impl fmt::Debug for TextInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextInput")
            .field("len", &self.len())
            .field("cursor", &self.cursor)
            .field("masked", &self.masked)
            .finish()
    }
}

impl TextInput {
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the text (when the wizard changed it) and put the cursor at the end.
    pub fn set_value(&mut self, value: &str) {
        self.value = value.to_string();
        self.cursor = self.len();
    }

    /// Show one dot per character instead of the text.
    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    /// Apply an editing key. Returns true when the text changed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Char(c) => {
                self.insert(c);
                true
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_to(self.cursor.saturating_sub(1)),
            KeyCode::Right => self.move_to(self.cursor + 1),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.len()),
            _ => false,
        }
    }

    /// What the screen shows: the text, or one dot per character when masked.
    pub fn display(&self) -> String {
        if self.masked {
            "•".repeat(self.len())
        } else {
            self.value.clone()
        }
    }

    fn len(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(i, _)| i)
    }

    /// Cursor moves never change the text, so they report `false`.
    fn move_to(&mut self, cursor: usize) -> bool {
        self.cursor = cursor.min(self.len());
        false
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        let at = self.byte_index(self.cursor);
        self.value.remove(at);
        true
    }

    fn delete(&mut self) -> bool {
        if self.cursor >= self.len() {
            return false;
        }
        let at = self.byte_index(self.cursor);
        self.value.remove(at);
        true
    }
}

/// A labelled input: `label  value` on the first row, the error (or else the hint) below.
pub struct TextField<'a> {
    pub label: &'a str,
    pub input: &'a TextInput,
    pub focused: bool,
    pub error: Option<&'a str>,
    pub hint: Option<&'a str>,
    pub theme: &'a Theme,
}

impl TextField<'_> {
    /// Where the terminal cursor goes when this field has focus.
    pub fn cursor_position(&self, area: Rect) -> Position {
        let room = area.width.saturating_sub(LABEL_WIDTH + 1);
        let cursor = u16::try_from(self.input.cursor())
            .unwrap_or(u16::MAX)
            .min(room);
        Position::new(area.x + LABEL_WIDTH + cursor, area.y)
    }
}

impl Widget for TextField<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let label_style = if self.focused {
            self.theme.accent
        } else {
            self.theme.text
        };
        let width = usize::from(LABEL_WIDTH);
        let value = Line::from(vec![
            Span::styled(format!("{:<width$}", self.label), label_style),
            Span::styled(self.input.display(), self.theme.text),
        ]);
        let note = match (self.error, self.hint) {
            (Some(error), _) => Line::styled(format!("{:width$}{error}", ""), self.theme.error),
            (None, Some(hint)) => Line::styled(format!("{:width$}{hint}", ""), self.theme.muted),
            (None, None) => Line::default(),
        };
        Paragraph::new(vec![value, note]).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{SIZES, ctrl, key, render_lines};

    fn typed(text: &str) -> TextInput {
        let mut input = TextInput::default();
        for c in text.chars() {
            assert!(input.handle_key(key(KeyCode::Char(c))));
        }
        input
    }

    #[test]
    fn typing_inserts_and_backspace_deletes() {
        let mut input = typed("me@x.io");
        assert_eq!(input.value(), "me@x.io");
        assert!(input.handle_key(key(KeyCode::Backspace)));
        assert_eq!(input.value(), "me@x.i");
        assert_eq!(input.cursor(), 6);
    }

    #[test]
    fn cursor_moves_and_inserts_in_the_middle() {
        let mut input = typed("ac");
        assert!(!input.handle_key(key(KeyCode::Left)));
        assert!(input.handle_key(key(KeyCode::Char('b'))));
        assert_eq!(input.value(), "abc");
        assert!(!input.handle_key(key(KeyCode::Home)));
        assert_eq!(input.cursor(), 0);
        assert!(
            !input.handle_key(key(KeyCode::Backspace)),
            "nothing before the cursor"
        );
        assert!(input.handle_key(key(KeyCode::Delete)));
        assert_eq!(input.value(), "bc");
        assert!(!input.handle_key(key(KeyCode::End)));
        assert_eq!(input.cursor(), 2);
        assert!(!input.handle_key(key(KeyCode::Right)), "already at the end");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn editing_counts_characters_not_bytes() {
        let mut input = typed("né");
        assert_eq!(input.cursor(), 2);
        assert!(input.handle_key(key(KeyCode::Backspace)));
        assert_eq!(input.value(), "n");
    }

    #[test]
    fn control_keys_do_not_edit() {
        let mut input = typed("ab");
        assert!(!input.handle_key(ctrl('a')));
        assert!(!input.handle_key(key(KeyCode::Enter)));
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn set_value_puts_the_cursor_at_the_end() {
        let mut input = TextInput::default();
        input.set_value("work");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn masked_mode_shows_one_dot_per_character() {
        let mut input = typed("hunter2");
        input.set_masked(true);
        assert_eq!(input.display(), "•••••••");
        assert_eq!(input.value(), "hunter2");
    }

    #[test]
    fn debug_never_prints_the_value() {
        let mut input = typed("hunter2");
        input.set_masked(true);
        let shown = format!("{input:?}");
        assert!(!shown.contains("hunter2"), "debug output leaked the value");
        assert!(shown.contains('7'), "{shown}");
    }

    #[test]
    fn field_renders_label_value_and_error_at_both_sizes() {
        let theme = Theme::default();
        let input = typed("me@example");
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| {
                let field = TextField {
                    label: "Email address",
                    input: &input,
                    focused: true,
                    error: Some("not an email address"),
                    hint: Some("never shown while there is an error"),
                    theme: &theme,
                };
                frame.render_widget(field, frame.area());
            });
            assert!(
                lines[0].starts_with("Email address  me@example"),
                "{lines:?}"
            );
            assert!(lines[1].contains("not an email address"), "{lines:?}");
            assert!(!lines[1].contains("never shown"), "{lines:?}");
        }
    }

    #[test]
    fn hint_renders_when_there_is_no_error() {
        let theme = Theme::default();
        let input = TextInput::default();
        let lines = render_lines(80, 24, |frame| {
            let field = TextField {
                label: "Colour",
                input: &input,
                focused: false,
                error: None,
                hint: Some("optional, for example blue"),
                theme: &theme,
            };
            frame.render_widget(field, frame.area());
        });
        assert!(lines[1].contains("optional, for example blue"), "{lines:?}");
    }

    #[test]
    fn masked_field_never_renders_the_secret() {
        let theme = Theme::default();
        let mut input = typed("hunter2");
        input.set_masked(true);
        let lines = render_lines(80, 24, |frame| {
            let field = TextField {
                label: "Client secret",
                input: &input,
                focused: true,
                error: None,
                hint: None,
                theme: &theme,
            };
            frame.render_widget(field, frame.area());
        });
        let text = lines.join("\n");
        assert!(text.contains("•••••••"), "masked dots missing");
        assert!(!text.contains("hunter2"), "the secret was rendered");
    }

    #[test]
    fn cursor_position_follows_the_cursor() {
        let theme = Theme::default();
        let input = typed("abc");
        let field = TextField {
            label: "Name",
            input: &input,
            focused: true,
            error: None,
            hint: None,
            theme: &theme,
        };
        let area = Rect::new(2, 5, 60, FIELD_HEIGHT);
        assert_eq!(
            field.cursor_position(area),
            Position::new(2 + LABEL_WIDTH + 3, 5)
        );
    }
}
