//! `:errors`: the session's recent errors, newest first (their text already escaped),
//! wrapped to the box and scrolled with the keys help uses. While some entry can be retried
//! an entry is highlighted and `Enter` sends its action again.

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::scroll::ScrollBox;
use super::{Command, ModalOutcome, boxed};
use crate::tui::text::wrap;
use crate::tui::theme::Theme;

/// One `:errors` entry: its escaped text, and the outbox row `Enter` retries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRow {
    pub text: String,
    pub retry: Option<rail_core::ops::OutboxId>,
}

pub struct ErrorList {
    rows: Vec<ErrorRow>,
    scroll: ScrollBox,
    /// The highlighted entry; used only while some entry can be retried.
    selected: usize,
}

impl ErrorList {
    #[cfg(test)]
    pub fn new(rows: Vec<String>, icons: Icons) -> Self {
        Self::with_retries(
            rows.into_iter()
                .map(|text| ErrorRow { text, retry: None })
                .collect(),
            icons,
        )
    }

    pub fn with_retries(rows: Vec<ErrorRow>, icons: Icons) -> Self {
        Self {
            rows,
            scroll: ScrollBox::new(icons),
            selected: 0,
        }
    }

    fn retryable(&self) -> bool {
        self.rows.iter().any(|r| r.retry.is_some())
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        if !self.retryable() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => ModalOutcome::Close,
                _ => {
                    self.scroll.key(key);
                    ModalOutcome::Stay
                }
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ModalOutcome::Close,
            KeyCode::Enter => match self.rows.get(self.selected).and_then(|r| r.retry) {
                Some(row) => ModalOutcome::Run(Command::Retry(row)),
                None => ModalOutcome::Close,
            },
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
                ModalOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                ModalOutcome::Stay
            }
            _ => ModalOutcome::Stay,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = area.height.saturating_sub(4);
        let inner = boxed(frame, area, 86, height, "Recent errors", theme);
        if self.rows.is_empty() {
            let none = Line::styled("No errors this session.", theme.muted);
            frame.render_widget(Paragraph::new(none), inner);
            return;
        }
        let cols = usize::from(inner.width);
        let (mut lines, mut span) = (Vec::new(), (0, 0));
        for (i, row) in self.rows.iter().enumerate() {
            let selected = self.retryable() && i == self.selected;
            let style = if selected { theme.selected } else { theme.text };
            let start = lines.len();
            let text = match row.retry {
                Some(_) => format!("{} (Enter retries)", row.text),
                None => row.text.clone(),
            };
            lines.extend(
                wrap(&text, cols)
                    .into_iter()
                    .map(|l| Line::styled(l, style)),
            );
            if selected {
                span = (start, lines.len().saturating_sub(1));
            }
        }
        if self.retryable() {
            self.scroll
                .render_revealing(frame, inner, lines, theme, span.0, span.1);
        } else {
            self.scroll.render(frame, inner, lines, theme);
        }
    }
}
