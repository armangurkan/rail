//! The one optional modal over the mail screen: confirmations, the link number prompt, the
//! attachment list, the filter line, the palette, the label and theme pickers, help and `:errors`.
//! Modal keys are fixed (Enter, Esc, `y`/`n`, arrows): they are not keymap actions.

pub mod confirm;
pub mod errors;
pub mod filter;
pub mod help;
pub mod palette;
pub mod picker;
mod scroll;
#[cfg(test)]
mod tests;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear};
use url::Url;

pub use confirm::{AttachmentList, AttachmentRow, Confirm, LinkPrompt};
pub use errors::{ErrorList, ErrorRow};
pub use filter::FilterLine;
pub use help::Help;
pub use palette::{Palette, PaletteEntry, Target};
pub use picker::{AccentAnswer, Picker, PickerItem, PickerKind};

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
    /// Open the message with this id in the browser, once confirmed.
    OpenHtml(i64),
    /// `Esc` in the filter line: drop the filter.
    ClearFilter,
    /// Run what a palette entry stands for.
    Palette(Target),
    /// The trash confirmation was answered yes.
    Trash,
    /// Send this failed outbox row again.
    Retry(rail_core::ops::RowToken),
    /// The label picker's answer: labels to add and to remove.
    Labels {
        add: Vec<i64>,
        remove: Vec<i64>,
    },
    /// The move picker's answer: the label folder to move to.
    MoveTo(i64),
    /// `:theme`'s answer: an index into `theme::scheme::schemes()`, or `None` for Rail's default.
    Theme(Option<usize>),
    /// `:accent`'s answer.
    Accent(AccentAnswer),
    /// The account picker's answer: write a new message from account `n` (its position).
    Account(usize),
    /// The drafts picker's answer: open this draft in the composer.
    ResumeDraft(rail_core::ops::DraftId),
    /// "Keep what you wrote?" was answered yes: use the text the editor left.
    KeepText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalOutcome {
    Stay,
    Close,
    /// The filter text changed; the line stays open.
    Typed(String),
    Run(Command),
    /// The highlight moved in a picker that previews: apply `Command` without keeping it.
    Preview(Command),
}

pub enum Modal {
    Confirm(Confirm),
    LinkNumber(LinkPrompt),
    Attachments(AttachmentList),
    Filter(FilterLine),
    Palette(Palette),
    Picker(Picker),
    Help(Help),
    Errors(ErrorList),
}

impl Modal {
    pub fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match self {
            Self::Confirm(confirm) => confirm.key(key),
            Self::LinkNumber(prompt) => prompt.key(key),
            Self::Attachments(list) => list.key(key),
            Self::Filter(line) => line.key(key),
            Self::Palette(palette) => palette.key(key),
            Self::Picker(picker) => picker.key(key),
            Self::Help(help) => help.key(key),
            Self::Errors(list) => list.key(key),
        }
    }

    /// True while the modal takes typed text (so `q` is typed, not quit).
    pub fn captures_text(&self) -> bool {
        matches!(
            self,
            Self::LinkNumber(_) | Self::Filter(_) | Self::Palette(_) | Self::Picker(_)
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        match self {
            Self::Confirm(confirm) => confirm.render(frame, area, theme),
            Self::LinkNumber(prompt) => prompt.render(frame, area, theme),
            Self::Attachments(list) => list.render(frame, area, theme),
            Self::Filter(line) => line.render(frame, area, theme),
            Self::Palette(palette) => palette.render(frame, area, theme),
            Self::Picker(picker) => picker.render(frame, area, theme),
            Self::Help(help) => help.render(frame, area, theme),
            Self::Errors(list) => list.render(frame, area, theme),
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
