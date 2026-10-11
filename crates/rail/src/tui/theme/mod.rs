//! Named styles. One default theme; theme configuration is out of scope until Phase 7.

// Nothing outside the tests reads the table until the theme picker lands; drop this then.
#[allow(dead_code)]
pub mod scheme;
#[cfg(test)]
mod scheme_tests;

use ratatui::style::{Color, Modifier, Style};

/// The styles every screen draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub text: Style,
    pub muted: Style,
    pub accent: Style,
    pub error: Style,
    pub selected: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Style::new(),
            muted: Style::new().fg(Color::DarkGray),
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            selected: Style::new().fg(Color::Black).bg(Color::Cyan),
        }
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
