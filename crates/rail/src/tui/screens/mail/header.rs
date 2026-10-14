//! The header row: the scope and its counts on the left, the search slot in the middle, the
//! sync summary on the right. The parts are computed as text first (`Header`, `header_cells`)
//! so the width rules are tested without a terminal.

use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::view::Glyphs;
use super::{MailScreen, Summary};
use crate::tui::keys::Action;
use crate::tui::text::{escape, fit, width};
use crate::tui::theme::Theme;

/// Under this width the search slot is not drawn; under `MIN_FOR_RIGHT` the summary is not.
const MIN_FOR_SLOT: usize = 60;
const MIN_FOR_RIGHT: usize = 40;
/// A slot narrower than this is dropped rather than drawn.
const MIN_SLOT: usize = 8;
/// Spaces on each side of the slot.
const GAP: usize = 2;

/// `22,853`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the middle of the header shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Slot {
    /// The Drafts pane: no filter.
    None,
    /// `/ Search`, muted.
    Idle(String),
    /// `/ is:unread · from:dana`, in the accent style; also the text while the line is typed.
    Active(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Header {
    pub left: String,
    /// The selected threads, drawn after `left` in the text style.
    pub selected: Option<u64>,
    pub slot: Slot,
    pub right: Summary,
    /// The glyph set: the search region's prefix and the box's lines.
    pub icons: Icons,
}

impl Header {
    /// `left` and the selection count, joined: what the width rules measure.
    pub(super) fn joined(&self) -> String {
        match self.selected {
            Some(n) => format!("{} · {} selected", self.left, thousands(n)),
            None => self.left.clone(),
        }
    }
}

/// The header fitted to a width: the left text, the search region's prefix, the slot's column
/// (after the prefix) and text, the right text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderCells {
    pub left: String,
    /// `│ 🔍 ` before the slot, drawn only with it; empty when the slot is not drawn.
    pub prefix: String,
    pub slot: Option<(usize, String)>,
    pub right: Option<String>,
}

/// Under 40 columns only the left part is drawn. Otherwise the summary is kept whole (cut only
/// when it alone is wider than the row) and the left part is cut to the columns left beside it.
pub(super) fn header_cells(header: &Header, cols: usize) -> HeaderCells {
    let joined = header.joined();
    if cols < MIN_FOR_RIGHT {
        return HeaderCells {
            left: fit(&joined, cols),
            prefix: String::new(),
            slot: None,
            right: None,
        };
    }
    let right = fit(&header.right.text, cols);
    let right_w = width(&right);
    let left = fit(&joined, cols.saturating_sub(right_w + GAP));
    let left_w = width(&left);
    let prefix = Glyphs::of(header.icons).search;
    let prefix_w = width(prefix);
    let room = cols.saturating_sub(left_w + right_w + 2 * GAP + prefix_w);
    let slot = match &header.slot {
        Slot::Idle(text) | Slot::Active(text) if cols >= MIN_FOR_SLOT && room >= MIN_SLOT => {
            Some((left_w + GAP + prefix_w, fit(text, room)))
        }
        _ => None,
    };
    HeaderCells {
        left,
        prefix: if slot.is_some() {
            prefix.to_string()
        } else {
            String::new()
        },
        slot,
        right: Some(right),
    }
}

/// The slot's spans after `used` columns, its rect for the filter line, and the column after
/// it; `None` when the slot is not drawn.
fn slot_spans(
    area: Rect,
    cells: &HeaderCells,
    slot: &Slot,
    theme: &Theme,
    used: usize,
) -> Option<(Vec<Span<'static>>, Rect, usize)> {
    let (at, text) = cells.slot.as_ref()?;
    let style = match slot {
        Slot::Active(_) => theme.accent,
        _ => theme.muted,
    };
    let prefix_at = at.saturating_sub(width(&cells.prefix));
    let spans = vec![
        Span::raw(" ".repeat(prefix_at.saturating_sub(used))),
        Span::styled(cells.prefix.clone(), theme.muted),
        Span::styled(text.clone(), style),
    ];
    let right_w = cells.right.as_deref().map_or(0, width);
    let slot_cols = usize::from(area.width).saturating_sub(at + right_w + GAP);
    let rect = Rect::new(
        area.x + u16::try_from(*at).unwrap_or(u16::MAX),
        area.y,
        u16::try_from(slot_cols).unwrap_or(u16::MAX),
        1,
    );
    Some((spans, rect, at + width(text)))
}

/// Draw `header` in `area` (one row); the slot's rect when it is drawn, for the filter line.
pub(super) fn render_header(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    header: &Header,
) -> Option<Rect> {
    let cols = usize::from(area.width);
    let cells = header_cells(header, cols);
    // The scope part is accent; whatever follows it (the selection count, or the `…` of a cut)
    // is plain text.
    let cut = cells.left != header.joined();
    let kept = cells.left.chars().count().saturating_sub(usize::from(cut));
    let scope_chars = header.left.chars().count().min(kept);
    let scope: String = cells.left.chars().take(scope_chars).collect();
    let rest: String = cells.left.chars().skip(scope_chars).collect();
    let mut spans = vec![Span::styled(scope, theme.accent)];
    if !rest.is_empty() {
        spans.push(Span::styled(rest, theme.text));
    }
    let mut used = width(&cells.left);
    let mut slot_rect = None;
    if let Some((slot, rect, end)) = slot_spans(area, &cells, &header.slot, theme, used) {
        spans.extend(slot);
        slot_rect = Some(rect);
        used = end;
    }
    if let Some(right) = &cells.right {
        spans.push(Span::raw(
            " ".repeat(cols.saturating_sub(used + width(right))),
        ));
        let style = if header.right.error {
            theme.error
        } else {
            theme.muted
        };
        spans.push(Span::styled(right.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    slot_rect
}

impl MailScreen {
    /// The header's three parts from the screen's state.
    pub(super) fn header(&self) -> Header {
        Header {
            left: self.scope_left(),
            selected: self.selected_count(),
            slot: self.slot(),
            right: self.status.summary(
                &self.accounts,
                &Glyphs::of(self.ctx.icons),
                &self.ctx.keymap,
                &self.ctx.clock,
            ),
            icons: self.ctx.icons,
        }
    }

    /// `▍Rail │ All inboxes · 2 accounts · 22,853 unread · 3 selected`.
    #[cfg(test)]
    pub(crate) fn header_left(&self) -> String {
        Header {
            left: self.scope_left(),
            selected: self.selected_count(),
            slot: Slot::None,
            right: Summary {
                text: String::new(),
                error: false,
            },
            icons: self.ctx.icons,
        }
        .joined()
    }

    /// The scope, its account count and its unread count.
    fn scope_left(&self) -> String {
        let glyphs = Glyphs::of(self.ctx.icons);
        let mut left = format!(
            "{}Rail {} {}",
            glyphs.bar,
            glyphs.separator,
            self.scope_title()
        );
        if self.scope.is_unified() {
            let n = u64::try_from(self.accounts.len()).unwrap_or(u64::MAX);
            let plural = if n == 1 { "" } else { "s" };
            left.push_str(&format!(" · {} account{plural}", thousands(n)));
        }
        if self.scope.counts_unread() {
            let unread = self.sidebar.unread(&self.scope);
            left.push_str(&format!(" · {} unread", thousands(unread)));
        }
        left
    }

    /// How many threads are selected, when any.
    fn selected_count(&self) -> Option<u64> {
        let n = self.actions.selection.len();
        (n > 0).then(|| u64::try_from(n).unwrap_or(u64::MAX))
    }

    /// The summary's text, for tests.
    #[cfg(test)]
    pub(crate) fn header_summary(&self) -> String {
        self.header().right.text
    }

    /// The slot's text, for tests (empty in the Drafts pane).
    #[cfg(test)]
    pub(crate) fn slot_text(&self) -> String {
        match self.slot() {
            Slot::None => String::new(),
            Slot::Idle(text) | Slot::Active(text) => text,
        }
    }

    fn slot(&self) -> Slot {
        if self.in_drafts() {
            return Slot::None;
        }
        let key = self.ctx.keymap.label(Action::ListFilter);
        if self.filter.is_empty() {
            return Slot::Idle(format!("{key} Search"));
        }
        let parsed = escape(&self.filter.describe().join(" · "));
        Slot::Active(format!("{key} {parsed}"))
    }
}
