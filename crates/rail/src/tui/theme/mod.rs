//! Named styles. A theme is the default look or one of the vendored schemes (`scheme`),
//! chosen by `choice`.

pub mod choice;
#[cfg(test)]
mod choice_tests;
pub mod scheme;
#[cfg(test)]
mod scheme_tests;

use ratatui::style::{Color, Modifier, Style};

use self::choice::{Accent, ColorDepth, ThemeChoice};

/// The styles every screen draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub text: Style,
    pub muted: Style,
    pub accent: Style,
    pub error: Style,
    pub selected: Style,
    /// Filled behind every screen; `None` leaves the terminal's own background.
    pub background: Option<Color>,
    /// Account dots for accounts without `color`, by position (cyan, magenta, green, yellow,
    /// blue, red in the default theme).
    pub account_colors: [Color; 6],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Style::new(),
            muted: Style::new().fg(Color::DarkGray),
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            selected: Style::new().fg(Color::Black).bg(Color::Cyan),
            background: None,
            account_colors: [
                Color::Cyan,
                Color::Magenta,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Red,
            ],
        }
    }
}

/// The palette slots the account dots take, in order: cyan, magenta, green, yellow, blue, red.
const ACCOUNT_SLOTS: [usize; 6] = [6, 5, 2, 3, 4, 1];

impl Theme {
    /// The theme for `choice` on a terminal of `depth`. Without truecolor the scheme is
    /// ignored and only a slot or named accent applies to the default look.
    pub fn from_choice(choice: ThemeChoice, depth: ColorDepth) -> Self {
        let mut theme = match (depth, choice.scheme) {
            (ColorDepth::TrueColor, Some(scheme)) => {
                let bg = scheme.background.color();
                let on_bg = |c: Color| Style::new().fg(c).bg(bg);
                let mut account_colors = [Color::Reset; 6];
                for (slot, out) in ACCOUNT_SLOTS.iter().zip(account_colors.iter_mut()) {
                    *out = scheme.palette[*slot].color();
                }
                Self {
                    text: on_bg(scheme.foreground.color()),
                    muted: on_bg(scheme.palette[8].color()),
                    accent: on_bg(scheme.palette[6].color()).add_modifier(Modifier::BOLD),
                    error: on_bg(scheme.palette[1].color()),
                    selected: Style::new()
                        .fg(scheme.selection_fg.color())
                        .bg(scheme.selection_bg.color()),
                    background: Some(bg),
                    account_colors,
                }
            }
            _ => Self::default(),
        };
        if let Some(color) = accent_color(choice, depth) {
            theme.accent = theme.accent.fg(color);
        }
        theme
    }

    /// The dot colour of the account at `index` when it has no colour of its own.
    pub fn account_color(&self, index: usize) -> Color {
        self.account_colors[index % self.account_colors.len()]
    }
}

/// The accent to paint over the mapped one, if any: a slot of the scheme (or the ANSI slot
/// without one), or a colour of the user's own.
fn accent_color(choice: ThemeChoice, depth: ColorDepth) -> Option<Color> {
    match (choice.accent, depth, choice.scheme) {
        (Accent::SchemeDefault, ..) => None,
        (Accent::Slot(n), ColorDepth::TrueColor, Some(scheme)) => {
            Some(scheme.palette[usize::from(n) % 16].color())
        }
        (Accent::Slot(n), ..) => Some(Color::Indexed(n)),
        // 24-bit colour is not safe on a terminal that did not say it has it.
        (Accent::Color(Color::Rgb(..)), ColorDepth::Ansi, _) => None,
        (Accent::Color(c), ..) => Some(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_tells_errors_and_accents_apart() {
        let theme = Theme::default();
        assert_eq!(theme.error.fg, Some(Color::Red));
        assert_ne!(theme.accent, theme.text);
        assert_ne!(theme.selected, theme.text);
        assert_ne!(theme.muted, theme.text);
    }
}
