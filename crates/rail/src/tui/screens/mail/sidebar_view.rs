//! Drawing the sidebar: the UNIFIED and ACCOUNTS headings, rules, the merged mailboxes, each
//! account's arrow and badge, folders with fold arrows and icons, counts on the right. The
//! cursor row is drawn in the selected style while the sidebar has focus, else the current
//! scope's row in the accent; the rows scroll to keep that row in view. Labels and folder names
//! are outside text: `pad` and `fit` escape them.

use rail_core::config::Icons;
use rail_core::provider::FolderKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::header::thousands;
use super::layout;
use super::sidebar_tree::{RowKind, SidebarRow};
use super::view::Glyphs;
use super::{AccountEntry, Focus, MailScreen};
use crate::tui::data::ScopeChoice;
use crate::tui::text::{fit, pad, width};
use crate::tui::theme::Theme;

/// Cells of the icon column.
const ICON_COLS: usize = 2;
/// The top of a sidebar whose last read failed.
const READ_FAILED: &str = "Could not read folders";

impl MailScreen {
    /// Boxed, the box is the border; stacked, a line on the right in the border style.
    pub(super) fn render_sidebar(
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
            stacked_rule(frame, area, theme, self.ctx.icons)
        };
        let cols = usize::from(inner.width);
        let note = self
            .sidebar
            .failed()
            .then(|| Line::styled(fit(READ_FAILED, cols), theme.error));
        let height = usize::from(inner.height).saturating_sub(usize::from(note.is_some()));
        let focused = self.focus == Focus::Sidebar;
        let cursor = self.sidebar.cursor();
        let current = self.sidebar.current(&self.scope);
        let target = if focused {
            cursor
        } else {
            current.unwrap_or(cursor)
        };
        let offset = self.sidebar.scroll(target, height);
        let rows = self
            .sidebar
            .rows()
            .iter()
            .enumerate()
            .skip(offset)
            .take(height);
        let lines = rows.map(|(i, row)| {
            let style = if focused && i == cursor {
                theme.selected
            } else if !focused && Some(i) == current {
                theme.accent
            } else {
                theme.text
            };
            self.sidebar_line(row, style, cols, theme, glyphs)
        });
        let lines: Vec<Line> = note.into_iter().chain(lines).collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// One row in `cols` cells, by its kind; `style` is the cursor's, the current scope's or
    /// the text's.
    fn sidebar_line(
        &self,
        row: &SidebarRow,
        style: Style,
        cols: usize,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Line<'static> {
        match &row.kind {
            RowKind::Heading(text) => {
                Line::styled(fit(text, cols), theme.muted.add_modifier(Modifier::BOLD))
            }
            RowKind::Rule => Line::styled(glyphs.rule.repeat(cols), theme.border),
            RowKind::Account { index } => {
                self.account_line(row, *index, style, cols, glyphs, theme)
            }
            RowKind::More { .. } if style == theme.text => {
                folder_line(row, theme.muted, cols, glyphs)
            }
            _ => folder_line(row, style, cols, glyphs),
        }
    }

    /// The arrow, the badge (` P ` in black on the account's colour), the label, the inbox
    /// count.
    fn account_line(
        &self,
        row: &SidebarRow,
        index: usize,
        style: Style,
        cols: usize,
        glyphs: &Glyphs,
        theme: &Theme,
    ) -> Line<'static> {
        let lead = lead(row, glyphs);
        let letter = self
            .accounts
            .get(index)
            .map_or_else(String::new, AccountEntry::badge);
        let badge = format!(" {letter} ");
        let colour = self.account_color(index, theme);
        let count = count_text(row);
        let gap = usize::from(!count.is_empty());
        let used = width(&lead) + width(&badge) + 1 + gap + width(&count);
        let room = cols.saturating_sub(used);
        let rest = format!(" {}{}{count}", pad(&row.label, room), " ".repeat(gap));
        Line::from(vec![
            Span::styled(lead, style),
            Span::styled(badge, Style::new().fg(Color::Black).bg(colour)),
            Span::styled(rest, style),
        ])
    }
}

/// Stacked, the sidebar's right edge is a line in the border style; the rest is its inside.
fn stacked_rule(frame: &mut Frame, area: Rect, theme: &Theme, icons: Icons) -> Rect {
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_set(layout::border_set(icons))
        .border_style(theme.border);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// A merged mailbox, folder, `More folders` or tree row: the lead, the icon, the label cut to
/// the room the count leaves, the count on the right.
fn folder_line(row: &SidebarRow, style: Style, cols: usize, glyphs: &Glyphs) -> Line<'static> {
    let head = format!("{}{} ", lead(row, glyphs), icon(row, glyphs));
    let count = count_text(row);
    let gap = usize::from(!count.is_empty());
    let room = cols.saturating_sub(width(&head) + gap + width(&count));
    let text = format!("{head}{}{}{count}", pad(&row.label, room), " ".repeat(gap));
    Line::from(Span::styled(text, style))
}

/// Two cells per depth beyond the first, then the fold arrow and a space (two spaces when the
/// row does not fold); nothing for a merged mailbox, which starts with its icon.
fn lead(row: &SidebarRow, glyphs: &Glyphs) -> String {
    if matches!(row.kind, RowKind::Unified(_)) {
        return String::new();
    }
    let indent = "  ".repeat(usize::from(row.depth.saturating_sub(1)));
    let arrow = match (row.foldable, row.folded) {
        (false, _) => "  ".to_string(),
        (true, true) => format!("{} ", glyphs.closed),
        (true, false) => format!("{} ", glyphs.open),
    };
    format!("{indent}{arrow}")
}

/// The two-cell icon column: a one-cell glyph is followed by a space; blank for a label or a
/// user folder with nothing under it.
fn icon(row: &SidebarRow, glyphs: &Glyphs) -> String {
    let glyph = match &row.kind {
        RowKind::Unified(scope) => match scope {
            ScopeChoice::Starred => glyphs.icon_starred,
            ScopeChoice::Snoozed => glyphs.icon_snoozed,
            ScopeChoice::Drafts => glyphs.icon_drafts,
            ScopeChoice::Sent => glyphs.icon_sent,
            ScopeChoice::Trash => glyphs.icon_trash,
            _ => glyphs.icon_inbox,
        },
        RowKind::Folder { kind, .. } => match kind {
            FolderKind::Inbox => glyphs.icon_inbox,
            FolderKind::Sent => glyphs.icon_sent,
            FolderKind::Drafts => glyphs.icon_drafts,
            FolderKind::Trash => glyphs.icon_trash,
            FolderKind::Archive => glyphs.icon_archive,
            FolderKind::Spam => glyphs.icon_spam,
            FolderKind::Label | FolderKind::User => "",
        },
        RowKind::Saved { .. } => glyphs.icon_search,
        RowKind::More { .. } | RowKind::Parent { .. } => glyphs.icon_folder,
        RowKind::Heading(_) | RowKind::Rule | RowKind::Account { .. } => "",
    };
    pad(glyph, ICON_COLS)
}

/// `22,853`; blank for none or 0.
fn count_text(row: &SidebarRow) -> String {
    row.count
        .filter(|&n| n > 0)
        .map(thousands)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use rail_core::config::Icons;
    use rail_core::provider::FolderKind;
    use ratatui::style::Style;
    use ratatui::text::Line;

    use super::*;
    use crate::tui::screens::mail::sidebar_tree::RowKey;

    fn leaf(label: &str, depth: u8, count: Option<u64>) -> SidebarRow {
        SidebarRow {
            kind: RowKind::Folder {
                account: 0,
                scope: ScopeChoice::Folder {
                    account: 0,
                    folder_id: 1,
                    name: label.into(),
                    kind: rail_core::provider::FolderKind::User,
                },
                kind: FolderKind::User,
            },
            key: RowKey::Folder {
                account: "w@example.com".into(),
                folder_id: 1,
            },
            label: label.into(),
            depth,
            count,
            foldable: false,
            folded: false,
            parents: Vec::new(),
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Review Focus 2.
    #[test]
    fn a_deep_row_cuts_its_label_first_and_keeps_its_count() {
        let glyphs = Glyphs::of(Icons::Unicode);
        let row = leaf("Linode-and-more", 7, Some(1_234));
        let shown = text(&folder_line(&row, Style::new(), 24, &glyphs));
        assert_eq!(width(&shown), 24, "{shown:?}");
        assert!(shown.ends_with(" 1,234"), "{shown:?}");
        assert!(shown.contains('…'), "{shown:?}");
        let tiny = text(&folder_line(
            &leaf("Linode", 7, Some(4)),
            Style::new(),
            3,
            &glyphs,
        ));
        assert!(
            tiny.ends_with('4'),
            "nothing panics at three columns: {tiny:?}"
        );
    }

    /// Review Focus 1 (the folder half).
    #[test]
    fn outside_text_in_a_row_is_escaped() {
        let row = leaf("Ev\u{1b}[2Jil", 2, None);
        let shown = text(&folder_line(
            &row,
            Style::new(),
            24,
            &Glyphs::of(Icons::Unicode),
        ));
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
        assert!(shown.contains("Ev\\u{1b}[2Jil"), "{shown:?}");
    }

    #[test]
    fn rows_lead_with_their_indent_and_arrow_and_counts_hide_zero() {
        let glyphs = Glyphs::of(Icons::Unicode);
        let mut parent = leaf("Infrastructure", 2, Some(7));
        parent.foldable = true;
        parent.folded = true;
        assert_eq!(lead(&parent, &glyphs), "  ▸ ");
        parent.folded = false;
        assert_eq!(lead(&parent, &Glyphs::of(Icons::Ascii)), "  v ");
        assert_eq!(lead(&leaf("Sent", 1, None), &glyphs), "  ");
        let unified = SidebarRow {
            kind: RowKind::Unified(ScopeChoice::Starred),
            ..leaf("Starred", 0, Some(12))
        };
        assert_eq!(
            lead(&unified, &glyphs),
            "",
            "unified rows start with their icon"
        );
        assert_eq!(count_text(&leaf("x", 1, Some(0))), "");
        assert_eq!(count_text(&leaf("x", 1, Some(22_853))), "22,853");
        let more = SidebarRow {
            kind: RowKind::More {
                account: 0,
                hidden: 3,
            },
            foldable: true,
            folded: true,
            ..leaf("More folders (3)", 1, None)
        };
        assert_eq!(count_text(&more), "", "the arrow says it is folded");
    }

    #[test]
    fn every_icon_takes_two_cells_in_both_sets() {
        for icons in [Icons::Unicode, Icons::Ascii] {
            let glyphs = Glyphs::of(icons);
            for kind in [
                FolderKind::Inbox,
                FolderKind::Sent,
                FolderKind::Drafts,
                FolderKind::Trash,
                FolderKind::Archive,
                FolderKind::Spam,
                FolderKind::Label,
                FolderKind::User,
            ] {
                let row = SidebarRow {
                    kind: RowKind::Folder {
                        account: 0,
                        scope: ScopeChoice::Account(0),
                        kind,
                    },
                    ..leaf("x", 1, None)
                };
                assert_eq!(width(&icon(&row, &glyphs)), ICON_COLS, "{kind:?}");
            }
        }
        let starred = SidebarRow {
            kind: RowKind::Unified(ScopeChoice::Starred),
            ..leaf("Starred", 0, None)
        };
        assert_eq!(icon(&starred, &Glyphs::of(Icons::Ascii)), "* ");
        assert_eq!(icon(&starred, &Glyphs::of(Icons::Unicode)), "★ ");
        let trash = SidebarRow {
            kind: RowKind::Unified(ScopeChoice::Trash),
            ..leaf("Trash", 0, None)
        };
        assert_eq!(icon(&trash, &Glyphs::of(Icons::Unicode)), "✕ ");
    }

    #[test]
    fn the_snoozed_mailbox_has_its_own_two_cell_icon_in_both_sets() {
        let snoozed = SidebarRow {
            kind: RowKind::Unified(ScopeChoice::Snoozed),
            ..leaf("Snoozed", 0, None)
        };
        for (icons, drawn) in [(Icons::Unicode, "◷ "), (Icons::Ascii, "^ ")] {
            let glyph = icon(&snoozed, &Glyphs::of(icons));
            assert_eq!((glyph.as_str(), width(&glyph)), (drawn, ICON_COLS));
        }
    }

    #[test]
    fn the_badge_is_the_labels_first_letter_upper_cased_and_escaped() {
        let mut account = crate::tui::test_util::work_and_home().remove(0);
        account.label = "personal".into();
        assert_eq!(account.badge(), "P");
        account.label = "\u{7}x".into();
        assert_eq!(
            account.badge(),
            "…",
            "an escaped control is wider than the badge"
        );
    }
}
