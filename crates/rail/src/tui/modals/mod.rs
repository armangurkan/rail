//! The one optional modal over the mail screen: confirmations, the link number prompt and the
//! attachment list here; the palette, help, the filter line and `:errors` join in Task 10.
//! Modal keys are fixed (Enter, Esc, `y`/`n`, arrows): they are not keymap actions.

pub mod confirm;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear};
use url::Url;

pub use confirm::{AttachmentList, AttachmentRow, Confirm, LinkPrompt};

use crate::tui::theme::Theme;

/// What a modal asks the screen to do once it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    OpenLink(Url),
    /// Show the address of link `number` (of `links`) for confirmation.
    ChooseLink {
        links: Vec<String>,
        number: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalOutcome {
    Stay,
    Close,
    Run(Command),
}

pub enum Modal {
    Confirm(Confirm),
    LinkNumber(LinkPrompt),
    Attachments(AttachmentList),
}

impl Modal {
    pub fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match self {
            Self::Confirm(confirm) => confirm.key(key),
            Self::LinkNumber(prompt) => prompt.key(key),
            Self::Attachments(list) => list.key(key),
        }
    }

    /// True while the modal takes typed text (so `q` is typed, not quit).
    pub fn captures_text(&self) -> bool {
        matches!(self, Self::LinkNumber(_))
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        match self {
            Self::Confirm(confirm) => confirm.render(frame, area, theme),
            Self::LinkNumber(prompt) => prompt.render(frame, area, theme),
            Self::Attachments(list) => list.render(frame, area, theme),
        }
    }
}

/// A bordered box of `width` x `height` (at most the area) in the middle of `area`, cleared,
/// with `title`; its inside.
pub(crate) fn boxed(
    frame: &mut Frame,
    area: Rect,
    width: u16,
    height: u16,
    title: &str,
    theme: &Theme,
) -> Rect {
    let [_, row, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, cell, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(row);
    frame.render_widget(Clear, cell);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .title(format!(" {title} "));
    let inner = block.inner(cell);
    frame.render_widget(block, cell);
    inner
}
