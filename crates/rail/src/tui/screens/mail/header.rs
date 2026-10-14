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
    /// The selected threads, drawn after `left` in the text style.
    pub selected: Option<u64>,
    pub slot: Slot,
    pub right: Summary,
}

impl Header {
    /// `left` and the selection count, joined: what the width rules measure.
    fn joined(&self) -> String {
        match self.selected {
            Some(n) => format!("{} · {} selected", self.left, thousands(n)),
            None => self.left.clone(),
        }
    }
}

/// The header fitted to a width: the left text, the slot's column and text, the right text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderCells {
    pub left: String,
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
            slot: None,
            right: None,
        };
    }
    let right = fit(&header.right.text, cols);
    let right_w = width(&right);
    let left = fit(&joined, cols.saturating_sub(right_w + GAP));
    let left_w = width(&left);
    let room = cols.saturating_sub(left_w + right_w + 2 * GAP);
    let slot = match &header.slot {
        Slot::Idle(text) | Slot::Active(text) if cols >= MIN_FOR_SLOT && room >= MIN_SLOT => {
            Some((left_w + GAP, fit(text, room)))
        }
        _ => None,
    };
    HeaderCells {
        left,
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
    let spans = vec![
        Span::raw(" ".repeat(at.saturating_sub(used))),
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
        if self.scope == ScopeChoice::Unified {
            let n = u64::try_from(self.accounts.len()).unwrap_or(u64::MAX);
            let plural = if n == 1 { "" } else { "s" };
            left.push_str(&format!(" · {} account{plural}", thousands(n)));
        }
        let unread = self.sidebar.unread(&self.scope);
        left.push_str(&format!(" · {} unread", thousands(unread)));
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rail_core::paths::Paths;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Style};

    use super::*;
    use crate::tui::data::FakeDaemon;
    use crate::tui::test_util::{mail_ctx, render_lines, work_and_home};

    fn header(left: &str, slot: Slot, right: &str) -> Header {
        Header {
            left: left.to_string(),
            selected: None,
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
        let h = Header {
            selected: Some(1_234),
            ..header("▍Rail │ All inboxes", Slot::None, "x")
        };
        assert_eq!(h.joined(), "▍Rail │ All inboxes · 1,234 selected");
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
        let long = header(
            &"y".repeat(50),
            Slot::Idle("/ Search".into()),
            "● Synced 11:40",
        );
        let cells = header_cells(&long, 59);
        assert!(cells.slot.is_none());
        assert_eq!(
            cells.right.as_deref(),
            Some("● Synced 11:40"),
            "the right stays whole"
        );
        assert_eq!(
            width(&cells.left),
            59 - 14 - 2,
            "the left is cut to what is left"
        );
        assert!(cells.left.ends_with('…'), "{:?}", cells.left);
        let cells = header_cells(&h, 39);
        assert!(cells.slot.is_none());
        assert!(cells.right.is_none());
        assert_eq!(cells.left, "▍Rail │ All inboxes");
        let wide = header(&"y".repeat(40), Slot::None, "● Synced 11:40");
        let cells = header_cells(&wide, 30);
        assert_eq!(width(&cells.left), 30, "the left part is cut to the width");
    }

    #[test]
    fn the_summary_outranks_the_scope_title_when_both_do_not_fit() {
        let h = Header {
            selected: Some(3),
            ..header(
                "▍Rail │ All inboxes · 2 accounts · 22,853 unread",
                Slot::Idle("/ Search".into()),
                "daemon reachable again (Ctrl-r resumes live updates)",
            )
        };
        let right = "daemon reachable again (Ctrl-r resumes live updates)";
        let cells = header_cells(&h, 90);
        assert_eq!(
            cells.right.as_deref(),
            Some(right),
            "the summary is kept whole"
        );
        assert_eq!(width(&cells.left), 90 - width(right) - 2);
        assert!(cells.left.starts_with("▍Rail │"), "{:?}", cells.left);
        assert!(cells.left.ends_with('…'), "{:?}", cells.left);
        assert!(cells.slot.is_none(), "no room is left for the slot");
        let cells = header_cells(&h, 45);
        assert_eq!(
            width(cells.right.as_deref().unwrap()),
            45,
            "a summary wider than the row is cut"
        );
        assert_eq!(cells.left, "");
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

    #[test]
    fn the_selection_count_is_drawn_in_the_text_style_not_the_accent() {
        let h = Header {
            selected: Some(3),
            ..header("▍Rail │ All inboxes", Slot::None, "● Synced 11:40")
        };
        let theme = Theme {
            text: Style::new().fg(Color::Green),
            ..Theme::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| {
                render_header(frame, frame.area(), &theme, &h);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row: String = (0..80).map(|x| buffer[(x, 0)].symbol()).collect();
        let byte = row.find("selected").unwrap();
        let at = u16::try_from(row[..byte].chars().count()).unwrap();
        assert_eq!(buffer[(at, 0)].symbol(), "s");
        assert_eq!(buffer[(at, 0)].fg, Color::Green);
        assert_eq!(buffer[(0, 0)].fg, theme.accent.fg.unwrap());
    }

    #[test]
    fn a_cut_scope_ends_in_an_ellipsis_in_the_text_style() {
        let h = header(&"y".repeat(60), Slot::None, "● Synced 11:40");
        let theme = Theme {
            text: Style::new().fg(Color::Green),
            ..Theme::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(50, 1)).unwrap();
        terminal
            .draw(|frame| {
                render_header(frame, frame.area(), &theme, &h);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let at = u16::try_from(50 - 14 - 2 - 1).unwrap();
        assert_eq!(buffer[(at, 0)].symbol(), "…");
        assert_eq!(buffer[(at, 0)].fg, Color::Green);
        assert_eq!(buffer[(0, 0)].fg, theme.accent.fg.unwrap());
        let tiny = render_lines(40, 1, |frame| {
            render_header(
                frame,
                frame.area(),
                &theme,
                &header("x", Slot::None, &"z".repeat(60)),
            );
        });
        assert_eq!(
            width(tiny[0].trim_end()),
            40,
            "a summary alone too wide fills the row"
        );
    }

    /// Review Focus 1 (the label half).
    #[test]
    fn a_label_names_the_account_on_screen_and_a_blank_one_falls_back_to_the_name() {
        let mut config = crate::tui::test_util::existing_config();
        config.accounts[0].label = Some("Personal".into());
        let mut second = config.accounts[0].clone();
        second.name = "home".into();
        second.address = "h@example.com".into();
        second.label = Some("  ".into());
        config.accounts.push(second);
        let mut accounts = crate::tui::screens::mail::AccountEntry::from_config(&config);
        assert_eq!(accounts[0].name, "work", "the name stays the identifier");
        assert_eq!(accounts[0].label, "Personal");
        assert_eq!(accounts[1].label, "home", "a blank label is no label");
        accounts[1].label = "Ev\u{7}il".into();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = mail_ctx(&Paths::all_in(tmp.path()), Arc::new(FakeDaemon::default()));
        let mut screen = MailScreen::new(ctx, accounts);
        screen.scope = crate::tui::data::ScopeChoice::Account(0);
        assert_eq!(screen.header_left(), "▍Rail │ Personal · Inbox · 0 unread");
        screen.scope = crate::tui::data::ScopeChoice::Account(1);
        assert_eq!(
            screen.header_left(),
            "▍Rail │ Ev\\u{7}il · Inbox · 0 unread"
        );
        let bar = screen.bar(&Theme::default());
        assert!(
            bar.right[0].1.starts_with("Personal"),
            "{:?}",
            bar.right[0].1
        );
        assert!(
            bar.right[1].1.starts_with("Ev\\u{7}il"),
            "{:?}",
            bar.right[1].1
        );
        let texts: Vec<String> = screen
            .palette_entries()
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert!(
            texts.contains(&"Go to Personal's inbox".to_string()),
            "{texts:?}"
        );
    }
}
