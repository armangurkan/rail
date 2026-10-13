//! The bottom row: key hints for the focused pane, built from the keymap, a notice in their
//! place while one shows, and each account's state on the right.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::status::account_part;
use super::view::Glyphs;
use super::{Focus, MailScreen};
use crate::tui::keys::{Action, Keymap};
use crate::tui::screens::compose::Composer;
use crate::tui::screens::compose::review::DISCARD_QUESTION;
use crate::tui::text::{escape, fit, width};
use crate::tui::theme::Theme;

/// Which hint set the bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pane {
    List,
    Drafts,
    Sidebar,
    Reader,
    Compose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hint {
    pub key: String,
    pub word: String,
}

/// Between two hints.
const SEP: &str = "  ";
/// Between two accounts on the right.
const ACCOUNT_SEP: &str = " · ";

fn hint(key: String, word: &str) -> Hint {
    Hint {
        key,
        word: word.to_string(),
    }
}

/// `j/k`: the two actions' first keys.
fn pair(keymap: &Keymap, a: Action, b: Action) -> String {
    format!("{}/{}", keymap.label(a), keymap.label(b))
}

/// The hints of `pane`, `? help` last; `provider` is where the composer's `s` saves.
pub(super) fn hints(pane: Pane, keymap: &Keymap, sidebar_shown: bool, provider: &str) -> Vec<Hint> {
    let l = |a: Action| keymap.label(a);
    let mut out = match pane {
        Pane::List => vec![
            hint(l(Action::ListOpen), "open"),
            hint(pair(keymap, Action::ListDown, Action::ListUp), "move"),
            hint(l(Action::SidebarFocus), "sidebar"),
            hint(l(Action::ListReply), "reply"),
            hint(l(Action::ListCompose), "compose"),
            hint(l(Action::ListArchive), "archive"),
            hint(l(Action::ListTrash), "trash"),
            hint(l(Action::ListFilter), "filter"),
        ],
        Pane::Drafts => vec![
            hint(l(Action::ListOpen), "open"),
            hint(l(Action::ListTrash), "discard"),
            hint(l(Action::SidebarFocus), "sidebar"),
        ],
        Pane::Sidebar => vec![
            hint(l(Action::SidebarOpen), "show"),
            hint(pair(keymap, Action::SidebarDown, Action::SidebarUp), "move"),
            hint(l(Action::SidebarFocus), "list"),
        ],
        Pane::Reader => vec![
            hint(
                pair(keymap, Action::ReaderNext, Action::ReaderPrev),
                "message",
            ),
            hint(l(Action::ReaderExpand), "fold"),
            hint(l(Action::ListReply), "reply"),
            hint(l(Action::ListArchive), "archive"),
            hint(l(Action::ReaderBack), "back"),
        ],
        Pane::Compose => vec![
            hint(l(Action::ComposeSend), "send"),
            hint(l(Action::ComposePush), &format!("save to {provider}")),
            hint(l(Action::ComposeEdit), "edit"),
            hint(l(Action::ComposeHeaders), "headers"),
            hint(l(Action::ComposeAttach), "attach"),
            hint(l(Action::ComposeRemove), "remove"),
            hint(l(Action::ComposeClose), "keep"),
            hint(l(Action::ComposeDiscard), "discard"),
        ],
    };
    if !sidebar_shown && matches!(pane, Pane::List | Pane::Drafts) {
        out.retain(|h| h.word != "sidebar");
    }
    out.push(hint(l(Action::GlobalHelp), "help"));
    out
}

fn one_width(h: &Hint) -> usize {
    width(&h.key) + 1 + width(&h.word)
}

/// The cells `hints` take, separators included.
#[cfg(test)]
pub(super) fn hint_width(hints: &[Hint]) -> usize {
    let text: usize = hints.iter().map(one_width).sum();
    text + width(SEP) * hints.len().saturating_sub(1)
}

/// The hints that fit in `room`: whole hints from the left, the last one (`? help`) always.
pub(super) fn fit_hints(hints: &[Hint], room: usize) -> Vec<Hint> {
    let Some((help, rest)) = hints.split_last() else {
        return Vec::new();
    };
    let mut kept: Vec<Hint> = Vec::new();
    let mut used = one_width(help);
    for h in rest {
        let next = used + one_width(h) + width(SEP);
        if next > room {
            break;
        }
        used = next;
        kept.push(h.clone());
    }
    kept.push(help.clone());
    kept
}

/// What the bar draws.
pub(super) struct Bar<'a> {
    pub hints: Vec<Hint>,
    /// A notice in place of the hints, and whether it is an error.
    pub notice: Option<(&'a str, bool)>,
    /// The glyph before each account's state.
    pub dot: &'static str,
    /// Each account's dot colour and (escaped) state text.
    pub right: Vec<(Color, String)>,
}

impl Bar<'_> {
    /// The accounts joined, as one string.
    fn right_text(&self) -> String {
        let parts: Vec<String> = self
            .right
            .iter()
            .map(|(_, text)| format!("{} {text}", self.dot))
            .collect();
        parts.join(ACCOUNT_SEP)
    }
}

/// The right part: coloured per account when it fits whole, else cut as one muted string.
fn right_spans(bar: &Bar<'_>, theme: &Theme, room: usize) -> Vec<Span<'static>> {
    let whole = bar.right_text();
    if width(&whole) > room {
        return vec![Span::styled(fit(&whole, room), theme.muted)];
    }
    let mut spans = Vec::new();
    for (i, (color, text)) in bar.right.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(ACCOUNT_SEP, theme.muted));
        }
        spans.push(Span::styled(
            format!("{} ", bar.dot),
            theme.muted.fg(*color),
        ));
        spans.push(Span::styled(text.clone(), theme.muted));
    }
    spans
}

/// The left part: the notice, or the hints that fit in `room`.
fn left_spans(bar: &Bar<'_>, theme: &Theme, room: usize) -> Vec<Span<'static>> {
    if let Some((text, error)) = bar.notice {
        let style = if error { theme.error } else { theme.muted };
        return vec![Span::styled(fit(text, room), style)];
    }
    let mut spans = Vec::new();
    for (i, h) in fit_hints(&bar.hints, room).iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(SEP));
        }
        spans.push(Span::styled(h.key.clone(), theme.selected));
        spans.push(Span::styled(format!(" {}", h.word), theme.muted));
    }
    spans
}

/// Draw the bar: the left part, then the accounts at the right edge. The accounts give way
/// first, down to what leaves room for `? help`.
pub(super) fn render_hints(frame: &mut Frame, area: Rect, theme: &Theme, bar: &Bar<'_>) {
    let cols = usize::from(area.width);
    let help_w = bar.hints.last().map_or(0, one_width);
    let right_room = cols.saturating_sub(help_w + width(SEP));
    let right = right_spans(bar, theme, width(&bar.right_text()).min(right_room));
    let right_w: usize = right.iter().map(|s| width(&s.content)).sum();
    let gap = if right_w > 0 { width(SEP) } else { 0 };
    let mut spans = left_spans(bar, theme, cols.saturating_sub(right_w + gap));
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    spans.push(Span::raw(" ".repeat(cols.saturating_sub(used + right_w))));
    spans.extend(right);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

impl MailScreen {
    pub(super) fn pane(&self) -> Pane {
        match self.focus {
            Focus::Compose => Pane::Compose,
            Focus::Sidebar => Pane::Sidebar,
            Focus::Reader => Pane::Reader,
            Focus::List if self.in_drafts() => Pane::Drafts,
            Focus::List => Pane::List,
        }
    }

    /// The bar from the screen's state: the focused pane's hints, the notice, the accounts.
    pub(super) fn bar(&self, theme: &Theme) -> Bar<'_> {
        let glyphs = Glyphs::of(self.ctx.icons);
        let provider = self.compose.as_ref().map_or("", |c| {
            self.kind_at(c.composing().account_index).provider_name()
        });
        let hints = hints(
            self.pane(),
            &self.ctx.keymap,
            self.sidebar_visible(),
            provider,
        );
        let right = self
            .accounts
            .iter()
            .enumerate()
            .map(|(i, account)| {
                let status = self.status.status_of(account);
                let text = account_part(&escape(&account.name), status, &glyphs);
                (self.account_color(i, theme), text)
            })
            .collect();
        let discarding = self
            .compose
            .as_ref()
            .is_some_and(Composer::confirming_discard);
        let notice = if discarding {
            Some((DISCARD_QUESTION, true))
        } else {
            self.status
                .notice()
                .map(|t| (t, self.status.notice_is_error()))
        };
        Bar {
            hints,
            notice,
            dot: glyphs.account,
            right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::keymap;

    fn words(hints: &[Hint]) -> Vec<String> {
        hints
            .iter()
            .map(|h| format!("{} {}", h.key, h.word))
            .collect()
    }

    #[test]
    fn each_pane_has_its_hints_in_order() {
        let km = keymap();
        assert_eq!(
            words(&hints(Pane::List, &km, true, "Gmail")),
            [
                "Enter open",
                "j/k move",
                "Tab sidebar",
                "r reply",
                "c compose",
                "e archive",
                "d trash",
                "/ filter",
                "? help"
            ]
        );
        assert_eq!(
            words(&hints(Pane::Sidebar, &km, true, "Gmail")),
            ["Enter show", "j/k move", "Tab list", "? help"]
        );
        assert_eq!(
            words(&hints(Pane::Reader, &km, true, "Gmail")),
            [
                "n/p message",
                "Tab fold",
                "r reply",
                "e archive",
                "q back",
                "? help"
            ]
        );
        assert_eq!(
            words(&hints(Pane::Drafts, &km, true, "Gmail")),
            ["Enter open", "d discard", "Tab sidebar", "? help"]
        );
        assert_eq!(
            words(&hints(Pane::Compose, &km, true, "Microsoft 365")),
            [
                "y send",
                "s save to Microsoft 365",
                "e edit",
                "t headers",
                "a attach",
                "x remove",
                "q keep",
                "d discard",
                "? help"
            ]
        );
    }

    #[test]
    fn a_hidden_sidebar_drops_its_hint_and_a_remapped_key_shows_its_label() {
        let km = keymap();
        assert!(
            !words(&hints(Pane::List, &km, false, "Gmail")).contains(&"Tab sidebar".to_string())
        );
        assert!(
            !words(&hints(Pane::Drafts, &km, false, "Gmail")).contains(&"Tab sidebar".to_string())
        );
        let text = "[keys]\n\"list.archive\" = \"A\"\n";
        let path = std::path::Path::new("/c/config.toml");
        let config = rail_core::config::parse(text, path).unwrap();
        let km = crate::tui::keys::Keymap::from_config(&config.keys, text, path).unwrap();
        assert!(words(&hints(Pane::List, &km, true, "Gmail")).contains(&"A archive".to_string()));
    }

    #[test]
    fn trimming_keeps_whole_hints_and_help_last() {
        let km = keymap();
        let all = hints(Pane::List, &km, true, "Gmail");
        let full = hint_width(&all);
        assert_eq!(fit_hints(&all, full).len(), all.len());
        let fewer = fit_hints(&all, full - 1);
        assert_eq!(
            fewer.len(),
            all.len() - 1,
            "one hint drops, not a character"
        );
        assert_eq!(fewer.last().unwrap().word, "help");
        assert_eq!(
            fewer[fewer.len() - 2].word,
            "trash",
            "the one before help drops"
        );
        let tiny = fit_hints(&all, 20);
        assert_eq!(words(&tiny), ["Enter open", "? help"]);
        let only_help = fit_hints(&all, 6);
        assert_eq!(words(&only_help), ["? help"]);
    }

    fn draw_bar(bar: &Bar<'_>, cols: u16) -> String {
        let theme = Theme::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, 1)).unwrap();
        terminal
            .draw(|frame| render_hints(frame, frame.area(), &theme, bar))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..cols)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }

    fn sample_bar(notice: Option<(&'static str, bool)>) -> Bar<'static> {
        Bar {
            hints: hints(Pane::List, &keymap(), true, "Gmail"),
            notice,
            dot: "●",
            right: vec![
                (Color::Green, "work ✓".to_string()),
                (Color::Red, "home ⟳".to_string()),
            ],
        }
    }

    #[test]
    fn the_bar_draws_hints_left_and_accounts_right() {
        let row = draw_bar(&sample_bar(None), 80);
        assert!(row.starts_with("Enter open  j/k move"), "{row}");
        assert!(row.ends_with("● work ✓ · ● home ⟳"), "{row}");
        assert!(row.contains("? help"), "{row}");
        assert_eq!(width(&row), 80);
    }

    #[test]
    fn a_notice_replaces_the_hints_and_a_narrow_bar_keeps_help() {
        let row = draw_bar(&sample_bar(Some(("Archived", false))), 80);
        assert!(row.starts_with("Archived"), "{row}");
        assert!(!row.contains("Enter open"), "{row}");
        let narrow = draw_bar(&sample_bar(None), 30);
        assert!(narrow.contains("? help"), "{narrow}");
        assert_eq!(width(&narrow), 30);
    }

    fn bar_buffer(bar: &Bar<'_>, cols: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, 1)).unwrap();
        terminal
            .draw(|frame| render_hints(frame, frame.area(), &Theme::default(), bar))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn accounts_too_wide_for_the_room_are_cut_as_one_muted_string() {
        let bar = sample_bar(None);
        let row = draw_bar(&bar, 20);
        assert!(row.starts_with("? help"), "help keeps its room: {row}");
        assert!(row.contains("● work"), "{row}");
        assert!(!row.contains("home"), "the right part is cut: {row}");
        assert_eq!(width(&row), 20);
        let buffer = bar_buffer(&bar, 20);
        let dot = (0..20u16)
            .find(|x| buffer[(*x, 0)].symbol() == "●")
            .unwrap();
        assert_eq!(
            buffer[(dot, 0)].fg,
            Theme::default().muted.fg.unwrap(),
            "a cut part loses the account colours"
        );
    }

    #[test]
    fn an_error_notice_is_drawn_in_the_error_style() {
        let theme = Theme::default();
        let error = bar_buffer(&sample_bar(Some(("Could not archive", true))), 80);
        assert_eq!(error[(0, 0)].fg, theme.error.fg.unwrap());
        let plain = bar_buffer(&sample_bar(Some(("Archived", false))), 80);
        assert_eq!(plain[(0, 0)].fg, theme.muted.fg.unwrap());
    }

    #[test]
    fn account_names_in_the_bar_are_escaped() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::tui::test_util::mail_ctx(
            &rail_core::paths::Paths::all_in(tmp.path()),
            std::sync::Arc::new(crate::tui::data::FakeDaemon::default()),
        );
        let mut accounts = crate::tui::test_util::work_and_home();
        accounts[0].name = "wo\u{1b}[2Jrk".into();
        let screen = MailScreen::new(ctx, accounts);
        let bar = screen.bar(&Theme::default());
        let text = &bar.right[0].1;
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("wo\\u{1b}[2Jrk"), "{text:?}");
    }
}
