//! The Drafts folder's pane: an account's local drafts and the Gmail drafts no local draft
//! owns, newest first, drawn in the list area with the thread list's columns. A local-only
//! draft says `(local)` where the people go and has the draft glyph before its subject.

use std::cell::Cell;

use rail_core::provider::Mailbox;
use rail_core::store::drafts_folder::{DraftsRow, DraftsRowKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::view::{DATE_COLS, Glyphs, participants, people_cols, visible_offset};
use super::{Focus, MailScreen};
use crate::tui::text::{fit, pad, pad_left};
use crate::tui::theme::Theme;

/// What a local draft no Gmail copy has shows in the people column.
const LOCAL_ONLY: &str = "(local)";

/// The pane's rows and cursor.
#[derive(Debug, Default)]
pub(super) struct DraftsPane {
    rows: Vec<DraftsRow>,
    cursor: usize,
    /// A read is in flight and nothing was read for this folder yet.
    loading: bool,
    /// The last read failed: the rows kept are older.
    failed: bool,
    /// The first row on screen, from the last frame.
    offset: Cell<usize>,
}

/// Two rows are the same draft (the cursor follows it across reads).
fn same(a: &DraftsRow, b: &DraftsRow) -> bool {
    match (a.kind, b.kind) {
        (DraftsRowKind::Local { draft: x, .. }, DraftsRowKind::Local { draft: y, .. }) => x == y,
        (DraftsRowKind::Remote { message: x }, DraftsRowKind::Remote { message: y }) => x == y,
        _ => false,
    }
}

impl DraftsPane {
    /// A folder opened: nothing to show until its read answers.
    pub(super) fn opened() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    pub(super) fn rows(&self) -> &[DraftsRow] {
        &self.rows
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn selected(&self) -> Option<&DraftsRow> {
        self.rows.get(self.cursor)
    }

    pub(super) fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    pub(super) fn top(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }

    /// Fresh rows: the cursor stays on its draft, or at its place when that one went.
    pub(super) fn apply(&mut self, rows: Vec<DraftsRow>) {
        let at = self
            .selected()
            .and_then(|old| rows.iter().position(|r| same(r, old)));
        self.cursor = at.unwrap_or(self.cursor).min(rows.len().saturating_sub(1));
        self.rows = rows;
        self.loading = false;
        self.failed = false;
    }

    /// The read failed: the rows stay.
    pub(super) fn fail(&mut self) {
        self.loading = false;
        self.failed = true;
    }

    /// The line drawn instead of rows, if any.
    fn message(&self) -> Option<&'static str> {
        if !self.rows.is_empty() {
            return None;
        }
        Some(if self.loading {
            "Loading…"
        } else if self.failed {
            "The drafts could not be read."
        } else {
            "No drafts here."
        })
    }
}

/// The people column of `row`, raw: `(local)`, or its recipients as the list names people.
fn people(row: &DraftsRow) -> String {
    if matches!(
        row.kind,
        DraftsRowKind::Local {
            on_remote: false,
            ..
        }
    ) {
        return LOCAL_ONLY.to_string();
    }
    let to: Vec<Mailbox> = row
        .to
        .iter()
        .map(|r| Mailbox {
            name: r.name.clone(),
            address: r.address.clone(),
        })
        .collect();
    participants(&to)
}

impl MailScreen {
    /// The pane in the list area.
    pub(super) fn render_drafts(&self, frame: &mut Frame, area: Rect, theme: &Theme, g: &Glyphs) {
        let pane = &self.drafts.pane;
        if let Some(text) = pane.message() {
            frame.render_widget(Paragraph::new(Line::styled(text, theme.muted)), area);
            return;
        }
        let height = usize::from(area.height);
        let offset = visible_offset(pane.offset.get(), pane.cursor(), height);
        pane.offset.set(offset);
        let lines: Vec<Line> = pane
            .rows()
            .iter()
            .enumerate()
            .skip(offset)
            .take(height)
            .map(|(i, row)| self.drafts_line(row, i == pane.cursor(), area.width, theme, g))
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn drafts_line(
        &self,
        row: &DraftsRow,
        selected: bool,
        cols: u16,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Line<'static> {
        let color = self
            .account_index_of(row.account_id)
            .map_or(Color::Reset, |i| self.account_color(i, theme));
        let style = if selected && self.focus != Focus::Sidebar {
            theme.selected
        } else {
            theme.text
        };
        let people_cols = people_cols(cols);
        let date = pad_left(&self.ctx.clock.short(row.date), DATE_COLS);
        let people = pad(&people(row), people_cols);
        let mark = match row.kind {
            DraftsRowKind::Local {
                on_remote: false, ..
            } => glyphs.draft,
            _ => " ",
        };
        let used = 2 + DATE_COLS + 2 + people_cols + 2 + 2;
        let room = usize::from(cols).saturating_sub(used);
        let subject = if row.subject.trim().is_empty() {
            "(no subject)"
        } else {
            row.subject.as_str()
        };
        Line::from(vec![
            Span::styled(format!("{} ", glyphs.account), style.fg(color)),
            Span::styled(format!("{date}  {people}  {mark} "), style),
            Span::styled(fit(subject, room), style),
        ])
    }
}
