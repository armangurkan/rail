//! What the user chose: a scheme (or none) and an accent, read from `[general]` and checked
//! against the vendored table at startup; and the terminal's colour depth.

use std::path::Path;

use rail_core::config::{AccentSpec, General};
use ratatui::style::Color;

use super::scheme::{Scheme, nearest, scheme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    SchemeDefault,
    Slot(u8),
    Color(Color),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeChoice {
    pub scheme: Option<&'static Scheme>,
    pub accent: Accent,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        Self {
            scheme: None,
            accent: Accent::SchemeDefault,
        }
    }
}

/// Whether the terminal shows 24-bit colour: `COLORTERM` is `truecolor` or `24bit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi,
}

pub fn color_depth(var: impl Fn(&str) -> Option<String>) -> ColorDepth {
    match var("COLORTERM").as_deref() {
        Some("truecolor" | "24bit") => ColorDepth::TrueColor,
        _ => ColorDepth::Ansi,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("{path}:{line}: no theme {name:?}{suggestion}")]
    UnknownTheme {
        path: String,
        line: usize,
        name: String,
        /// Empty, or `; did you mean "Dracula"?`.
        suggestion: String,
    },
    #[error("{path}:{line}: accent {value:?} is not a colour name or #rrggbb")]
    BadAccent {
        path: String,
        line: usize,
        value: String,
    },
}

impl ThemeChoice {
    /// Resolve `[general]`'s `theme` and `accent`. `text` is the config file, for line numbers.
    pub fn from_config(general: &General, text: &str, path: &Path) -> Result<Self, ThemeError> {
        let shown = path.display().to_string();
        let scheme = match &general.theme {
            None => None,
            Some(name) => Some(scheme(name).ok_or_else(|| ThemeError::UnknownTheme {
                path: shown.clone(),
                line: key_line(text, "theme"),
                name: name.clone(),
                suggestion:
                    nearest(name).map_or(String::new(), |n| format!("; did you mean {n:?}?")),
            })?),
        };
        let accent = match &general.accent {
            None => Accent::SchemeDefault,
            Some(AccentSpec::Slot(n)) => Accent::Slot(*n),
            Some(AccentSpec::Color(value)) => {
                Accent::Color(value.parse::<Color>().map_err(|_| ThemeError::BadAccent {
                    path: shown.clone(),
                    line: key_line(text, "accent"),
                    value: value.clone(),
                })?)
            }
        };
        Ok(Self { scheme, accent })
    }
}

/// The 1-based line of the first `key =` at the start of a line, or 0 when not found.
fn key_line(text: &str, key: &str) -> usize {
    text.lines()
        .position(|l| {
            let l = l.trim_start();
            l.starts_with(key) && l[key.len()..].trim_start().starts_with('=')
        })
        .map_or(0, |i| i + 1)
}
