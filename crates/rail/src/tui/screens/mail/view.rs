//! Drawing the mail screen: the header, the sidebar, the list, the reader pane (a placeholder
//! until a thread is open), each boxed in split mode, and the hint bar. Every outside string
//! goes through `tui::text`.

use rail_core::config::Icons;
use rail_core::provider::Mailbox;
use rail_core::store::view::ThreadRow;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::layout::{self, areas};
use super::sidebar::SidebarItem;
use super::{Focus, MailScreen, StoreState};
use super::{header, hints};
use crate::tui::data::ScopeChoice;
use crate::tui::keys::Action;
use crate::tui::modals::Modal;
use crate::tui::text::{escape, fit, outside, pad, pad_left, width};
use crate::tui::theme::Theme;

/// Columns of the date column.
pub(super) const DATE_COLS: usize = 10;
/// Columns of the message count (`(12)`).
const COUNT_COLS: usize = 5;

/// The glyphs of one `general.icons` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub bar: &'static str,
    pub account: &'static str,
    /// Replaces the account dot on a selected row.
    pub marked: &'static str,
    pub star: &'static str,
    /// Two columns wide in both sets.
    pub attachment: &'static str,
    pub ok: &'static str,
    pub syncing: &'static str,
    pub problem: &'static str,
    /// An expanded message's arrow.
    pub open: &'static str,
    /// A collapsed message's arrow.
    pub closed: &'static str,
    /// Before a draft kept only on this computer, in the Drafts folder.
    pub draft: &'static str,
    /// Between `Rail` and the scope in the header.
    pub separator: &'static str,
}

impl Glyphs {
    pub fn of(icons: Icons) -> Self {
        match icons {
            Icons::Unicode => Self {
                bar: "▍",
                account: "●",
                marked: "▸",
                star: "★",
                attachment: "📎",
                ok: "✓",
                syncing: "⟳",
                problem: "!",
                open: "▾",
                closed: "▸",
                draft: "✎",
                separator: "│",
            },
            Icons::Ascii => Self {
                bar: "|",
                account: "*",
                marked: ">",
                star: "+",
                attachment: "@ ",
                ok: "ok",
                syncing: "~",
                problem: "!",
                open: "v",
                closed: ">",
                draft: "*",
                separator: "|",
            },
        }
    }
}

/// The first row on screen: the remembered offset, moved just enough to show `selected`.
pub(crate) fn visible_offset(offset: usize, selected: usize, height: usize) -> usize {
    let height = height.max(1);
    if selected < offset {
        selected
    } else if selected >= offset + height {
        selected + 1 - height
    } else {
        offset
    }
}

/// Columns of the people column in a list `cols` wide.
pub(super) fn people_cols(cols: u16) -> usize {
    usize::from(cols / 4).clamp(12, 24)
}

/// Senders as the list shows them: names, or the part of the address before `@`.
pub(super) fn participants(people: &[Mailbox]) -> String {
    people
        .iter()
        .map(|p| match p.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => p.address.split('@').next().unwrap_or_default().to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl MailScreen {
    pub(super) fn render(&self, frame: &mut Frame, theme: &Theme) {
        let area = frame.area();
        let glyphs = Glyphs::of(self.ctx.icons);
        let sidebar = self
            .sidebar_shown
            .unwrap_or_else(|| layout::sidebar_by_default(area.width));
        let split = layout::is_split(area.width, self.ctx.split_threshold);
        let boxed = layout::bordered(area.width, self.ctx.split_threshold);
        let parts = areas(area, sidebar, split, self.hints);
        self.size.set((Self::list_rows(&parts, boxed), area.width));
        // An OSC 8 cell claims the cells after it, so the diff skips them; a modal drawn over
        // them could lose its border. While a modal is open the markers are plain `[n]`.
        let hyperlinks = self.ctx.hyperlinks && self.modal.is_none();
        let slot = header::render_header(frame, parts.header, theme, &self.header());
        self.slot_rect.set(slot);
        if let Some(composer) = &self.compose {
            composer.render(frame, parts.body, theme, &glyphs);
        } else {
            match (self.reader.as_ref(), parts.reader) {
                (Some(reader), Some(pane)) => {
                    self.render_browse(frame, &parts, boxed, theme, &glyphs);
                    let focused = self.focus == Focus::Reader;
                    let inner = self.boxed(frame, pane, boxed, focused, theme);
                    reader.render(frame, inner, theme, hyperlinks, focused);
                }
                (Some(reader), None) => reader.render(frame, parts.body, theme, hyperlinks, true),
                (None, pane) => {
                    self.render_browse(frame, &parts, boxed, theme, &glyphs);
                    if let Some(pane) = pane {
                        let inner = self.boxed(frame, pane, boxed, false, theme);
                        self.render_pane(frame, inner, theme);
                    }
                }
            }
        }
        hints::render_hints(frame, parts.hints, theme, &self.bar(theme), self.ctx.icons);
        match &self.modal {
            Some(Modal::Filter(line)) => {
                let row = self.slot_rect.get().unwrap_or(parts.hints);
                line.render(frame, row, theme);
            }
            Some(modal) => modal.render(frame, area, theme),
            None => {}
        }
    }

    /// The rows the list has: inside its box in split mode.
    fn list_rows(parts: &layout::Areas, boxed: bool) -> u16 {
        if boxed && layout::fits_border(parts.list) {
            parts.list.height - 2
        } else {
            parts.list.height
        }
    }

    /// Box `area` in split mode (when it is big enough) and give back the inside.
    fn boxed(
        &self,
        frame: &mut Frame,
        area: Rect,
        boxed: bool,
        focused: bool,
        theme: &Theme,
    ) -> Rect {
        if !boxed || !layout::fits_border(area) {
            return area;
        }
        let block = layout::pane_block(theme, self.ctx.icons, focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    }

    /// The sidebar and the list, each boxed in split mode.
    fn render_browse(
        &self,
        frame: &mut Frame,
        parts: &layout::Areas,
        boxed: bool,
        theme: &Theme,
        glyphs: &Glyphs,
    ) {
        if let Some(side) = parts.sidebar {
            let focused = self.focus == Focus::Sidebar;
            let inner = self.boxed(frame, side, boxed, focused, theme);
            self.render_sidebar(frame, inner, boxed, theme, glyphs);
        }
        let inner = self.boxed(frame, parts.list, boxed, self.focus == Focus::List, theme);
        self.render_list(frame, inner, theme, glyphs);
    }

    pub(super) fn scope_title(&self) -> String {
        let name = |i: usize| {
            self.accounts
                .get(i)
                .map_or("?".to_string(), |a| escape(&a.label))
        };
        match &self.scope {
            ScopeChoice::Account(i) => format!("{} · Inbox", name(*i)),
            ScopeChoice::Folder {
                account,
                name: folder,
                ..
            } => format!("{} · {}", name(*account), escape(folder)),
            merged => merged.unified_name().unwrap_or_default().to_string(),
        }
    }

    /// Boxed, the box is the border; stacked, a line on the right in the border style.
    fn render_sidebar(
        &self,
        frame: &mut Frame,
        area: Rect,
        boxed: bool,
        theme: &Theme,
        glyphs: &Glyphs,
    ) {
        let inner = if boxed {
            area
        } else {
            let block = Block::new()
                .borders(Borders::RIGHT)
                .border_set(layout::border_set(self.ctx.icons))
                .border_style(theme.border);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            inner
        };
        let cols = usize::from(inner.width);
        let items: Vec<Line> = self
            .sidebar
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| self.sidebar_line(index, item, cols, theme, glyphs))
            .collect();
        let note = self
            .sidebar
            .failed()
            .then(|| Line::styled(fit("Could not read folders", cols), theme.error));
        let lines: Vec<Line> = note.into_iter().chain(items).collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// One sidebar row: the account dot, the indented label, the unread count.
    fn sidebar_line(
        &self,
        index: usize,
        item: &SidebarItem,
        cols: usize,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Line<'static> {
        let selected = index == self.sidebar.cursor() && self.focus == super::Focus::Sidebar;
        let style = if selected { theme.selected } else { theme.text };
        let count = if item.unread > 0 {
            item.unread.to_string()
        } else {
            String::new()
        };
        let indent = if item.depth > 0 { "    " } else { "" };
        let dot = match item.account {
            Some(_) if item.depth == 0 => format!("{} ", glyphs.account),
            _ if item.depth == 0 => "  ".to_string(),
            _ => String::new(),
        };
        let color = item
            .account
            .map_or(Color::Reset, |i| self.account_color(i, theme));
        let label_cols = cols.saturating_sub(width(&dot) + indent.len() + count.len() + 1);
        Line::from(vec![
            Span::styled(dot, style.fg(color)),
            Span::styled(
                format!("{indent}{} {count}", pad(&item.label, label_cols)),
                style,
            ),
        ])
    }

    fn render_list(&self, frame: &mut Frame, area: Rect, theme: &Theme, glyphs: &Glyphs) {
        if self.in_drafts() {
            self.render_drafts(frame, area, theme, glyphs);
            return;
        }
        if let Some(lines) = self.list_message(theme) {
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
            return;
        }
        let height = usize::from(area.height);
        let selected = self.list.selected_index();
        let offset = visible_offset(self.list.offset(), selected, height);
        let rows: Vec<Line> = self
            .list
            .rows()
            .iter()
            .enumerate()
            .skip(offset)
            .take(height)
            .map(|(index, row)| self.row_line(row, index == selected, area.width, theme, glyphs))
            .collect();
        frame.render_widget(Paragraph::new(rows), area);
    }

    fn list_message(&self, theme: &Theme) -> Option<Vec<Line<'static>>> {
        let lines = match &self.store_state {
            StoreState::Loading => vec![Line::styled("Loading…", theme.muted)],
            StoreState::NotSetUp => vec![
                Line::styled("No mail here yet: the store is not set up.", theme.text),
                Line::styled(
                    "Run `rail sync` to fetch your mail; this list fills in as it arrives.",
                    theme.muted,
                ),
            ],
            StoreState::Failed(error) => {
                let mut lines = vec![Line::styled(
                    format!("The store could not be read: {}", escape(&error.message)),
                    theme.error,
                )];
                if let Some(detail) = &error.detail {
                    lines.push(Line::styled(outside(detail.get()), theme.muted));
                }
                if let Some(hint) = &error.hint {
                    lines.push(Line::styled(format!("hint: {}", escape(hint)), theme.muted));
                }
                lines
            }
            StoreState::Ready if self.list.rows().is_empty() => {
                let text = if self.filter.is_empty() {
                    "No threads here."
                } else {
                    "No threads match the filter."
                };
                vec![Line::styled(text, theme.muted)]
            }
            StoreState::Ready => return None,
        };
        Some(lines)
    }

    fn row_line(
        &self,
        row: &ThreadRow,
        selected: bool,
        cols: u16,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Line<'static> {
        let color = self
            .account_index(row.account_id)
            .map_or(Color::Reset, |i| self.account_color(i, theme));
        let base = if selected && self.focus != Focus::Sidebar {
            theme.selected
        } else {
            theme.text
        };
        let style = if row.unread_count > 0 {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let people_cols = people_cols(cols);
        let date = pad_left(&self.ctx.clock.short(row.key.last_date), DATE_COLS);
        let people = pad(&participants(&row.participants), people_cols);
        let count = if row.message_count > 1 {
            pad_left(&format!("({})", row.message_count), COUNT_COLS)
        } else {
            " ".repeat(COUNT_COLS)
        };
        let star = if row.starred { glyphs.star } else { " " };
        let clip = if row.has_attachments {
            glyphs.attachment
        } else {
            "  "
        };
        let used = 2 + DATE_COLS + 2 + people_cols + COUNT_COLS + 1 + 1 + 2 + 1;
        let room = usize::from(cols).saturating_sub(used);
        let (subject, subject_style) = match self.actions.failures.get(&row.key.thread_id) {
            Some((_, text)) => (fit(text, room), theme.error),
            None => (fit(&row.subject, room), style),
        };
        let lead = if self.actions.selection.contains(row.key.thread_id) {
            glyphs.marked
        } else {
            glyphs.account
        };
        Line::from(vec![
            Span::styled(format!("{lead} "), style.fg(color)),
            Span::styled(format!("{date}  {people}{count} {star}{clip} "), style),
            Span::styled(subject, subject_style),
        ])
    }

    fn account_index(&self, account_id: i64) -> Option<usize> {
        self.account_ids
            .iter()
            .position(|id| *id == Some(account_id))
    }

    /// The split layout's pane before a thread is open.
    fn render_pane(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let key = self.ctx.keymap.label(Action::ListOpen);
        let hint = if self.in_drafts() {
            format!("Open a draft with {key}; its review takes the screen.")
        } else {
            format!("Open a thread with {key}; it shows here.")
        };
        frame.render_widget(Paragraph::new(Line::styled(hint, theme.muted)), area);
    }
}
