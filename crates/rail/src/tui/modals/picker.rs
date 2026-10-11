//! The label pickers: `l` toggles any number of the account's labels (`Space` ticks, `Enter`
//! applies the difference from what the threads carried), `m` moves to one (`Enter`). Typing
//! filters with the palette's fuzzy matcher; label names are outside text, escaped when drawn.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{Command, ModalOutcome, boxed};
use crate::tui::text::{escape, input_row, pad};
use crate::tui::theme::Theme;
use crate::tui::widgets::text_input::TextInput;

/// Rows of labels shown.
const SHOWN: usize = 12;
/// The picker's width, its border included.
const PICKER_WIDTH: u16 = 60;

/// What a label picker does with its choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Toggle,
    Move,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub folder_id: i64,
    /// Outside text.
    pub name: String,
    pub checked: bool,
}

/// Ids and flags only: the label's name is mail content.
impl std::fmt::Debug for PickerItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerItem")
            .field("folder_id", &self.folder_id)
            .field("checked", &self.checked)
            .finish_non_exhaustive()
    }
}

struct Candidate<'a> {
    index: usize,
    text: &'a str,
}

impl AsRef<str> for Candidate<'_> {
    fn as_ref(&self) -> &str {
        self.text
    }
}

pub struct Picker {
    mode: PickerMode,
    items: Vec<PickerItem>,
    initial: BTreeSet<i64>,
    input: TextInput,
    matcher: Matcher,
    /// Indices of the matching items, best first.
    shown: Vec<usize>,
    selected: usize,
}

impl Picker {
    pub fn new(mode: PickerMode, items: Vec<PickerItem>) -> Self {
        let initial = items
            .iter()
            .filter(|i| i.checked)
            .map(|i| i.folder_id)
            .collect();
        let shown = (0..items.len()).collect();
        Self {
            mode,
            items,
            initial,
            input: TextInput::default(),
            matcher: Matcher::new(Config::DEFAULT),
            shown,
            selected: 0,
        }
    }

    fn refilter(&mut self) {
        let query = self.input.value();
        self.shown = if query.trim().is_empty() {
            (0..self.items.len()).collect()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let candidates = self.items.iter().enumerate().map(|(index, i)| Candidate {
                index,
                text: &i.name,
            });
            pattern
                .match_list(candidates, &mut self.matcher)
                .into_iter()
                .map(|(candidate, _)| candidate.index)
                .collect()
        };
        self.selected = 0;
    }

    fn enter(&self) -> ModalOutcome {
        match self.mode {
            PickerMode::Move => self
                .shown
                .get(self.selected)
                .and_then(|&i| self.items.get(i))
                .map_or(ModalOutcome::Stay, |item| {
                    ModalOutcome::Run(Command::MoveTo(item.folder_id))
                }),
            PickerMode::Toggle => {
                let now: BTreeSet<i64> = self
                    .items
                    .iter()
                    .filter(|i| i.checked)
                    .map(|i| i.folder_id)
                    .collect();
                ModalOutcome::Run(Command::Labels {
                    add: now.difference(&self.initial).copied().collect(),
                    remove: self.initial.difference(&now).copied().collect(),
                })
            }
        }
    }

    fn step(&mut self, down: bool) {
        self.selected = if down {
            (self.selected + 1).min(self.shown.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }

    /// `Down`/`Up` and `Ctrl-n`/`Ctrl-p` move (typed letters filter, so `j` and `k` cannot).
    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => ModalOutcome::Close,
            KeyCode::Enter => self.enter(),
            KeyCode::Down => {
                self.step(true);
                ModalOutcome::Stay
            }
            KeyCode::Up => {
                self.step(false);
                ModalOutcome::Stay
            }
            KeyCode::Char('n') if ctrl => {
                self.step(true);
                ModalOutcome::Stay
            }
            KeyCode::Char('p') if ctrl => {
                self.step(false);
                ModalOutcome::Stay
            }
            KeyCode::Char(' ') if self.mode == PickerMode::Toggle => {
                if let Some(item) = self
                    .shown
                    .get(self.selected)
                    .and_then(|&i| self.items.get_mut(i))
                {
                    item.checked = !item.checked;
                }
                ModalOutcome::Stay
            }
            _ => {
                if self.input.handle_key(key) {
                    self.refilter();
                }
                ModalOutcome::Stay
            }
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let (title, hint) = match self.mode {
            PickerMode::Toggle => ("Labels", "Space ticks · Enter applies · Esc cancels"),
            PickerMode::Move => ("Move to", "Enter moves · Esc cancels"),
        };
        let height = u16::try_from(SHOWN + 5).unwrap_or(u16::MAX);
        let inner = boxed(frame, area, PICKER_WIDTH, height, title, theme);
        let cols = usize::from(inner.width);
        let (typed, column) = input_row("> ", self.input.value(), self.input.cursor(), cols);
        let mut lines = vec![Line::styled(typed, theme.accent), Line::default()];
        let first = (self.selected + 1).saturating_sub(SHOWN);
        for (row, &index) in self.shown.iter().enumerate().skip(first).take(SHOWN) {
            let Some(item) = self.items.get(index) else {
                continue;
            };
            let style = if row == self.selected {
                theme.selected
            } else {
                theme.text
            };
            let mark = match (self.mode, item.checked) {
                (PickerMode::Move, _) => "",
                (PickerMode::Toggle, true) => "[x] ",
                (PickerMode::Toggle, false) => "[ ] ",
            };
            lines.push(Line::styled(
                pad(&format!("{mark}{}", escape(&item.name)), cols),
                style,
            ));
        }
        if self.shown.is_empty() {
            lines.push(Line::styled("No label matches.", theme.muted));
        }
        // The hint stays on the last inside row however many labels there are.
        let used = lines.len();
        let last = usize::from(inner.height).saturating_sub(1);
        lines.extend((used..last).map(|_| Line::default()));
        lines.push(Line::styled(pad(hint, cols), theme.muted));
        frame.render_widget(Paragraph::new(lines), inner);
        if cols > 0 && inner.height > 0 {
            let x = inner
                .x
                .saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
            frame.set_cursor_position(Position::new(x, inner.y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(p: &mut Picker, code: KeyCode) -> ModalOutcome {
        p.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn debug_shows_ids_and_flags_not_the_label_name() {
        let shown = format!("{:?}", items()[0]);
        assert!(!shown.contains("Projects"), "{shown}");
        assert!(
            shown.contains("folder_id: 1") && shown.contains("checked: true"),
            "{shown}"
        );
    }

    fn items() -> Vec<PickerItem> {
        vec![
            PickerItem {
                folder_id: 1,
                name: "Projects".into(),
                checked: true,
            },
            PickerItem {
                folder_id: 2,
                name: "Receipts".into(),
                checked: false,
            },
            PickerItem {
                folder_id: 3,
                name: "Travel".into(),
                checked: false,
            },
        ]
    }

    #[test]
    fn toggle_mode_applies_what_changed_relative_to_the_start() {
        let mut p = Picker::new(PickerMode::Toggle, items());
        press(&mut p, KeyCode::Char(' ')); // Projects off
        for c in "rec".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        press(&mut p, KeyCode::Char(' ')); // Receipts on
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            ModalOutcome::Run(Command::Labels {
                add: vec![2],
                remove: vec![1]
            })
        );
    }

    #[test]
    fn move_mode_moves_to_the_highlighted_label_and_esc_cancels() {
        let mut p = Picker::new(PickerMode::Move, items());
        press(&mut p, KeyCode::Down);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            ModalOutcome::Run(Command::MoveTo(2))
        );
        assert_eq!(press(&mut p, KeyCode::Esc), ModalOutcome::Close);
        let mut none = Picker::new(PickerMode::Move, items());
        for c in "zzz".chars() {
            press(&mut none, KeyCode::Char(c));
        }
        assert_eq!(
            press(&mut none, KeyCode::Enter),
            ModalOutcome::Stay,
            "nothing matches"
        );
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_while_letters_filter() {
        let mut p = Picker::new(PickerMode::Move, items());
        p.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        p.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            ModalOutcome::Run(Command::MoveTo(3))
        );
        p.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            ModalOutcome::Run(Command::MoveTo(2))
        );
        press(&mut p, KeyCode::Char('j'));
        assert_eq!(p.input.value(), "j", "j is typed");
    }
}
