//! A single-choice list, and the checkbox the wizard uses for the administrator flag.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState, Paragraph, StatefulWidget, Widget};

use crate::tui::theme::Theme;

/// A list with one highlighted item that Enter chooses.
#[derive(Debug, Clone, Default)]
pub struct Select {
    items: Vec<String>,
    state: ListState,
}

/// What a key did to a [`Select`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectOutcome {
    Moved,
    Chosen(usize),
    Ignored,
}

/// A step a [`Select`] takes. The screen turns keys into steps through the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Choose,
}

impl Select {
    pub fn new(items: Vec<String>) -> Self {
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(0));
        }
        Self { items, state }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// Replace the items, keeping the selection where it was when it still fits.
    pub fn set_items(&mut self, items: Vec<String>) {
        if items == self.items {
            return;
        }
        let selected = match self.state.selected() {
            _ if items.is_empty() => None,
            Some(i) => Some(i.min(items.len() - 1)),
            None => Some(0),
        };
        self.state.select(selected);
        self.items = items;
    }

    /// Up and Down move, Choose picks the highlighted item.
    pub fn handle(&mut self, nav: Nav) -> SelectOutcome {
        match nav {
            Nav::Up => self.step(false),
            Nav::Down => self.step(true),
            Nav::Choose => self
                .state
                .selected()
                .map_or(SelectOutcome::Ignored, SelectOutcome::Chosen),
        }
    }

    fn step(&mut self, down: bool) -> SelectOutcome {
        let Some(current) = self.state.selected() else {
            return SelectOutcome::Ignored;
        };
        let next = if down {
            (current + 1).min(self.items.len().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        if next == current {
            return SelectOutcome::Ignored;
        }
        self.state.select(Some(next));
        SelectOutcome::Moved
    }

    /// A renderable view; `focused` picks the highlight style.
    pub fn widget<'a>(&'a self, theme: &'a Theme, focused: bool) -> SelectView<'a> {
        SelectView {
            select: self,
            theme,
            focused,
        }
    }
}

/// The renderable form of a [`Select`].
pub struct SelectView<'a> {
    select: &'a Select,
    theme: &'a Theme,
    focused: bool,
}

impl Widget for SelectView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .select
            .items
            .iter()
            .map(|item| ListItem::new(item.as_str()))
            .collect();
        let highlight = if self.focused {
            self.theme.selected
        } else {
            self.theme.text
        };
        let list = List::new(items)
            .style(self.theme.text)
            .highlight_symbol("▸ ")
            .highlight_style(highlight);
        let mut state = self.select.state;
        StatefulWidget::render(list, area, buf, &mut state);
    }
}

/// `[x] label` or `[ ] label`.
pub struct Checkbox<'a> {
    pub label: &'a str,
    pub checked: bool,
    pub focused: bool,
    pub theme: &'a Theme,
}

impl Widget for Checkbox<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mark = if self.checked { "[x]" } else { "[ ]" };
        let style = if self.focused {
            self.theme.accent
        } else {
            self.theme.text
        };
        Paragraph::new(Line::styled(format!("{mark} {}", self.label), style)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{SIZES, render_lines};

    fn select() -> Select {
        Select::new(vec!["Gmail".into(), "Microsoft 365".into()])
    }

    #[test]
    fn arrows_move_and_enter_chooses() {
        let mut s = select();
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Chosen(0));
        assert_eq!(s.handle(Nav::Down), SelectOutcome::Moved);
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Chosen(1));
        assert_eq!(s.handle(Nav::Up), SelectOutcome::Moved);
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Chosen(0));
    }

    #[test]
    fn moving_stops_at_the_ends() {
        let mut s = select();
        assert_eq!(s.handle(Nav::Up), SelectOutcome::Ignored);
        s.handle(Nav::Down);
        assert_eq!(s.handle(Nav::Down), SelectOutcome::Ignored);
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Chosen(1));
    }

    #[test]
    fn an_empty_select_chooses_nothing() {
        let mut s = Select::new(Vec::new());
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Ignored);
        assert_eq!(s.handle(Nav::Down), SelectOutcome::Ignored);
    }

    #[test]
    fn set_items_keeps_the_selection_in_range() {
        let mut s = select();
        s.handle(Nav::Down);
        s.set_items(vec!["only".into()]);
        assert_eq!(s.items(), ["only".to_string()]);
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Chosen(0));
        s.set_items(Vec::new());
        assert_eq!(s.handle(Nav::Choose), SelectOutcome::Ignored);
    }

    #[test]
    fn renders_the_marker_on_the_selected_item_at_both_sizes() {
        let theme = Theme::default();
        let mut s = select();
        s.handle(Nav::Down);
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| {
                frame.render_widget(s.widget(&theme, true), frame.area());
            });
            assert!(lines[0].starts_with("  Gmail"), "{lines:?}");
            assert!(lines[1].starts_with("▸ Microsoft 365"), "{lines:?}");
        }
    }

    #[test]
    fn checkbox_shows_its_state() {
        let theme = Theme::default();
        for checked in [false, true] {
            let lines = render_lines(80, 24, |frame| {
                let checkbox = Checkbox {
                    label: "I am an administrator of this tenant",
                    checked,
                    focused: true,
                    theme: &theme,
                };
                frame.render_widget(checkbox, frame.area());
            });
            let mark = if checked { "[x]" } else { "[ ]" };
            assert!(
                lines[0].starts_with(&format!("{mark} I am an administrator")),
                "{lines:?}"
            );
        }
    }
}
