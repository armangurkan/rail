//! The header row: the scope and its counts on the left, the search slot in the middle, the
//! sync summary on the right. The parts are computed as text first (`Header`, `header_cells`)
//! so the width rules are tested without a terminal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::view::Glyphs;
use super::{MailScreen, Summary};
use crate::tui::data::ScopeChoice;
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
    pub slot: Slot,
    pub right: Summary,
}

/// The header fitted to a width: the left text, the slot's column and text, the right text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderCells {
    pub left: String,
    pub slot: Option<(usize, String)>,
    pub right: Option<String>,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "drawn from the frame task on; remove then")
)]
pub(super) fn header_cells(header: &Header, cols: usize) -> HeaderCells {
    let left_w = width(&header.left);
    let right_w = width(&header.right.text);
    if cols < MIN_FOR_RIGHT || left_w + GAP + right_w > cols {
        return HeaderCells {
            left: fit(&header.left, cols),
            slot: None,
            right: None,
        };
    }
    let room = cols.saturating_sub(left_w + right_w + 2 * GAP);
    let slot = match &header.slot {
        Slot::Idle(text) | Slot::Active(text) if cols >= MIN_FOR_SLOT && room >= MIN_SLOT => {
            Some((left_w + GAP, fit(text, room)))
        }
        _ => None,
    };
    HeaderCells {
        left: header.left.clone(),
        slot,
        right: Some(header.right.text.clone()),
    }
}

/// Draw `header` in `area` (one row); the slot's rect when it is drawn, for the filter line.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "drawn from the frame task on; remove then")
)]
pub(super) fn render_header(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    header: &Header,
) -> Option<Rect> {
    let cols = usize::from(area.width);
    let cells = header_cells(header, cols);
    let mut spans = vec![Span::styled(cells.left.clone(), theme.accent)];
    let mut used = width(&cells.left);
    let mut slot_rect = None;
    if let Some((at, text)) = &cells.slot {
        spans.push(Span::raw(" ".repeat(at.saturating_sub(used))));
        let style = match header.slot {
            Slot::Active(_) => theme.accent,
            _ => theme.muted,
        };
        spans.push(Span::styled(text.clone(), style));
        let slot_cols = cols.saturating_sub(at + width(&header.right.text) + GAP);
        slot_rect = Some(Rect::new(
            area.x + u16::try_from(*at).unwrap_or(u16::MAX),
            area.y,
            u16::try_from(slot_cols).unwrap_or(u16::MAX),
            1,
        ));
        used = at + width(text);
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "drawn from the frame task on; remove then")
    )]
    pub(super) fn header(&self) -> Header {
        Header {
            left: self.header_left(),
            slot: self.slot(),
            right: self.status.summary(
                &self.accounts,
                &Glyphs::of(self.ctx.icons),
                &self.ctx.keymap,
                &self.ctx.clock,
            ),
        }
    }

    /// `▍Rail │ All inboxes · 2 accounts · 22,853 unread · 3 selected`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "drawn from the frame task on; remove then")
    )]
    pub(crate) fn header_left(&self) -> String {
        let glyphs = Glyphs::of(self.ctx.icons);
        let mut left = format!("{}Rail │ {}", glyphs.bar, self.scope_title());
        if self.scope == ScopeChoice::Unified {
            let n = self.accounts.len();
            left.push_str(&format!(" · {n} account{}", if n == 1 { "" } else { "s" }));
        }
        let unread = self.sidebar.unread(&self.scope);
        left.push_str(&format!(" · {} unread", thousands(unread)));
        let selected = self.actions.selection.len();
        if selected > 0 {
            left.push_str(&format!(" · {selected} selected"));
        }
        left
    }

    /// The summary's text, for tests.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "drawn from the frame task on; remove then")
    )]
    pub(crate) fn header_summary(&self) -> String {
        self.header().right.text
    }

    /// The slot's text, for tests (empty in the Drafts pane).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "drawn from the frame task on; remove then")
    )]
    pub(crate) fn slot_text(&self) -> String {
        match self.slot() {
            Slot::None => String::new(),
            Slot::Idle(text) | Slot::Active(text) => text,
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "drawn from the frame task on; remove then")
    )]
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rail_core::paths::Paths;

    use super::*;
    use crate::tui::data::FakeDaemon;
    use crate::tui::test_util::{mail_ctx, render_lines, work_and_home};

    fn header(left: &str, slot: Slot, right: &str) -> Header {
        Header {
            left: left.to_string(),
            slot,
            right: Summary {
                text: right.to_string(),
                error: false,
            },
        }
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(22_853), "22,853");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn a_wide_header_has_the_slot_between_left_and_right() {
        let h = header(
            "▍Rail │ All inboxes",
            Slot::Idle("/ Search".into()),
            "● Synced 11:40",
        );
        let cells = header_cells(&h, 80);
        assert_eq!(cells.left, "▍Rail │ All inboxes");
        let (at, text) = cells.slot.unwrap();
        assert_eq!(at, width("▍Rail │ All inboxes") + 2);
        assert_eq!(text, "/ Search");
        assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
    }

    #[test]
    fn under_sixty_columns_the_slot_goes_and_under_forty_the_right_part_goes() {
        let h = header(
            "▍Rail │ All inboxes",
            Slot::Idle("/ Search".into()),
            "● Synced 11:40",
        );
        let cells = header_cells(&h, 59);
        assert!(cells.slot.is_none());
        assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
        let cells = header_cells(&h, 39);
        assert!(cells.slot.is_none());
        assert!(cells.right.is_none());
        assert_eq!(cells.left, "▍Rail │ All inboxes");
        let wide = header(&"y".repeat(40), Slot::None, "● Synced 11:40");
        let cells = header_cells(&wide, 30);
        assert_eq!(width(&cells.left), 30, "the left part is cut to the width");
    }

    #[test]
    fn a_long_filter_is_cut_from_the_right_and_a_tiny_slot_is_dropped() {
        let long = "x".repeat(100);
        let h = header("▍Rail │ All inboxes", Slot::Active(long), "● Synced 11:40");
        let (_, text) = header_cells(&h, 80).slot.unwrap();
        assert!(text.ends_with('…'));
        assert_eq!(
            width(&text),
            80 - width("▍Rail │ All inboxes") - width("● Synced 11:40") - 4
        );
        let h = header(
            &"y".repeat(40),
            Slot::Idle("/ Search".into()),
            "● Synced 11:40",
        );
        assert!(
            header_cells(&h, 64).slot.is_none(),
            "under 8 cells the slot is dropped"
        );
    }

    #[test]
    fn no_slot_leaves_the_middle_empty() {
        let h = header("▍Rail │ work · Drafts", Slot::None, "● Synced 11:40");
        let cells = header_cells(&h, 80);
        assert!(cells.slot.is_none());
        assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
    }

    fn bare_screen() -> (tempfile::TempDir, MailScreen) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = mail_ctx(&Paths::all_in(tmp.path()), Arc::new(FakeDaemon::default()));
        (tmp, MailScreen::new(ctx, work_and_home()))
    }

    #[test]
    fn the_screen_builds_its_header_from_its_state() {
        let (_tmp, screen) = bare_screen();
        assert_eq!(
            screen.header_left(),
            "▍Rail │ All inboxes · 2 accounts · 0 unread"
        );
        assert_eq!(screen.header_summary(), "connecting to the daemon…");
        assert_eq!(screen.slot_text(), "/ Search");
        assert_eq!(screen.header().slot, Slot::Idle("/ Search".into()));
    }

    #[test]
    fn the_header_is_drawn_with_the_slot_between_left_and_right() {
        let h = header(
            "▍Rail │ All inboxes",
            Slot::Idle("/ Search".into()),
            "● Synced 11:40",
        );
        let theme = Theme::default();
        let mut slot = None;
        let lines = render_lines(80, 1, |frame| {
            slot = render_header(frame, frame.area(), &theme, &h);
        });
        let row = &lines[0];
        assert!(row.starts_with("▍Rail │ All inboxes  / Search"), "{row:?}");
        assert!(row.trim_end().ends_with("● Synced 11:40"), "{row:?}");
        let slot = slot.expect("the slot is drawn at 80 columns");
        assert_eq!((slot.x, slot.y, slot.height), (21, 0, 1));
        let wide = render_lines(30, 1, |frame| {
            assert!(render_header(frame, frame.area(), &theme, &h).is_none());
        });
        assert_eq!(wide[0].trim_end(), "▍Rail │ All inboxes");
    }
}
