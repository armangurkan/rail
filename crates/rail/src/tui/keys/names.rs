//! Keys and their names. A name is a character (`a`, `O`, `/`), a named key (`enter`, `esc`,
//! `tab`, `backtab`, `space`, `up`, `down`, `left`, `right`, `pageup`, `pagedown`, `home`,
//! `end`, `f1` to `f12`), either with `ctrl-`, `alt-` and `shift-` before it, or two of those
//! with one space between them (a chord: `g x`). Names are read without regard to case,
//! except a character's own.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One key as the keymap compares it: Shift folded into the character (and into `BackTab`),
/// Ctrl and Alt kept, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let shift = mods.contains(KeyModifiers::SHIFT);
        let mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        let code = match code {
            KeyCode::Tab if shift => KeyCode::BackTab,
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
                KeyCode::Char(c.to_ascii_lowercase())
            }
            other => other,
        };
        Self { code, mods }
    }

    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }
}

/// What one action is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    Single(Key),
    /// Two keys pressed one after the other.
    Chord(Key, Key),
}

impl Binding {
    /// True when the binding presses `key` (alone or in a chord).
    pub fn uses(&self, key: Key) -> bool {
        match self {
            Self::Single(k) => *k == key,
            Self::Chord(a, b) => *a == key || *b == key,
        }
    }

    /// True when the two cannot share a view: the same key, the same chord, or a key that
    /// starts the other's chord.
    pub fn clashes(&self, other: &Binding) -> bool {
        match (self, other) {
            (Self::Single(a), Self::Single(b)) => a == b,
            (Self::Single(a), Self::Chord(b, _)) | (Self::Chord(b, _), Self::Single(a)) => a == b,
            (Self::Chord(a1, a2), Self::Chord(b1, b2)) => a1 == b1 && a2 == b2,
        }
    }
}

/// The key name `text`, if Rail can read it.
pub fn parse_binding(text: &str) -> Option<Binding> {
    let parts: Vec<&str> = text.split(' ').collect();
    match parts.as_slice() {
        [one] => parse_key(one).map(Binding::Single),
        [first, second] => Some(Binding::Chord(parse_key(first)?, parse_key(second)?)),
        _ => None,
    }
}

fn parse_key(text: &str) -> Option<Key> {
    let mut mods = KeyModifiers::NONE;
    let mut shift = false;
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(prefix) = ["ctrl-", "alt-", "shift-"]
            .into_iter()
            .find(|p| lower.starts_with(p) && rest.len() > p.len())
        else {
            break;
        };
        match prefix {
            "ctrl-" => mods |= KeyModifiers::CONTROL,
            "alt-" => mods |= KeyModifiers::ALT,
            _ => shift = true,
        }
        rest = &rest[prefix.len()..];
    }
    let code = named(rest).or_else(|| character(rest))?;
    let code = if shift { shifted(code)? } else { code };
    Some(Key::new(code, mods))
}

fn named(text: &str) -> Option<KeyCode> {
    let lower = text.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        f => {
            let number: u8 = f.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            KeyCode::F(number)
        }
    };
    Some(code)
}

/// One printable character that is not white space, a control or a Unicode format character
/// (zero-width marks, soft hyphens, bidirectional overrides, tags): none of those can be seen
/// or safely shown back to the user.
fn character(text: &str) -> Option<KeyCode> {
    let mut chars = text.chars();
    let c = chars.next()?;
    let printable = chars.next().is_none()
        && !c.is_whitespace()
        && !c.is_control()
        && !rail_core::config::is_format_char(c);
    printable.then_some(KeyCode::Char(c))
}

/// What `shift-` makes of a key: an upper-case letter, or `BackTab`. Nothing else.
fn shifted(code: KeyCode) -> Option<KeyCode> {
    match code {
        KeyCode::Char(c) if c.is_alphabetic() => {
            let mut upper = c.to_uppercase();
            let one = upper.next()?;
            upper.next().is_none().then_some(KeyCode::Char(one))
        }
        KeyCode::Tab => Some(KeyCode::BackTab),
        _ => None,
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("Ctrl-")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("Alt-")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::BackTab => f.write_str("Shift-Tab"),
            KeyCode::Up => f.write_str("↑"),
            KeyCode::Down => f.write_str("↓"),
            KeyCode::Left => f.write_str("←"),
            KeyCode::Right => f.write_str("→"),
            KeyCode::PageUp => f.write_str("PgUp"),
            KeyCode::PageDown => f.write_str("PgDn"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::F(n) => write!(f, "F{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(key) => write!(f, "{key}"),
            Self::Chord(first, second) => write!(f, "{first} {second}"),
        }
    }
}
