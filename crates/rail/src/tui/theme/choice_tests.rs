use std::path::Path;

use rail_core::config::{AccentSpec, General};
use ratatui::style::{Color, Modifier};

use super::Theme;
use super::choice::{Accent, ColorDepth, ThemeChoice, ThemeError, color_depth};
use super::scheme::scheme;

fn dracula() -> ThemeChoice {
    ThemeChoice {
        scheme: scheme("Dracula"),
        accent: Accent::SchemeDefault,
    }
}

#[test]
fn a_scheme_maps_onto_the_roles_by_slot() {
    let t = Theme::from_choice(dracula(), ColorDepth::TrueColor);
    let bg = Color::Rgb(0x28, 0x2a, 0x36);
    assert_eq!(t.background, Some(bg));
    assert_eq!(t.text.fg, Some(Color::Rgb(0xf8, 0xf8, 0xf2)));
    assert_eq!(t.text.bg, Some(bg));
    assert_eq!(t.muted.fg, Some(Color::Rgb(0x62, 0x72, 0xa4)), "palette 8");
    assert_eq!(t.accent.fg, Some(Color::Rgb(0x8b, 0xe9, 0xfd)), "palette 6");
    assert!(t.accent.add_modifier.contains(Modifier::BOLD));
    assert_eq!(t.error.fg, Some(Color::Rgb(0xff, 0x55, 0x55)), "palette 1");
    assert_eq!(t.border.fg, Some(Color::Rgb(0x62, 0x72, 0xa4)), "palette 8");
    assert_eq!(
        t.border_focus.fg,
        Some(Color::Rgb(0x8b, 0xe9, 0xfd)),
        "palette 6"
    );
    assert_eq!(t.selected.bg, Some(Color::Rgb(0x44, 0x47, 0x5a)));
    assert_eq!(t.selected.fg, Some(Color::Rgb(0xff, 0xff, 0xff)));
    assert_eq!(
        t.account_colors[0],
        Color::Rgb(0x8b, 0xe9, 0xfd),
        "cyan first"
    );
    assert_eq!(
        t.account_colors[5],
        Color::Rgb(0xff, 0x55, 0x55),
        "red last"
    );
    assert_eq!(t.account_color(7), t.account_colors[1], "wraps");
}

#[test]
fn the_accent_override_lands_in_accent_and_the_focus_border() {
    let base = Theme::from_choice(dracula(), ColorDepth::TrueColor);
    let slot = Theme::from_choice(
        ThemeChoice {
            accent: Accent::Slot(5),
            ..dracula()
        },
        ColorDepth::TrueColor,
    );
    assert_eq!(slot.accent.fg, Some(Color::Rgb(0xff, 0x79, 0xc6)));
    assert_eq!(slot.border_focus.fg, Some(Color::Rgb(0xff, 0x79, 0xc6)));
    assert_eq!(slot.border, base.border);
    assert_eq!(slot.text, base.text);
    assert_eq!(slot.selected, base.selected);
    assert_eq!(slot.account_colors, base.account_colors);
    let named = Theme::from_choice(
        ThemeChoice {
            accent: Accent::Color(Color::Blue),
            ..dracula()
        },
        ColorDepth::TrueColor,
    );
    assert_eq!(named.accent.fg, Some(Color::Blue));
}

#[test]
fn an_ansi_terminal_keeps_the_default_look_except_a_safe_accent() {
    let t = Theme::from_choice(dracula(), ColorDepth::Ansi);
    assert_eq!(t, Theme::default());
    let slot = Theme::from_choice(
        ThemeChoice {
            accent: Accent::Slot(5),
            ..dracula()
        },
        ColorDepth::Ansi,
    );
    assert_eq!(slot.accent.fg, Some(Color::Indexed(5)));
    assert_eq!(slot.background, None);
    let named = Theme::from_choice(
        ThemeChoice {
            scheme: None,
            accent: Accent::Color(Color::Blue),
        },
        ColorDepth::Ansi,
    );
    assert_eq!(named.accent.fg, Some(Color::Blue));
    let rgb = Theme::from_choice(
        ThemeChoice {
            scheme: None,
            accent: Accent::Color(Color::Rgb(1, 2, 3)),
        },
        ColorDepth::Ansi,
    );
    assert_eq!(
        rgb.accent.fg,
        Some(Color::Cyan),
        "an RGB accent is not applied on Ansi"
    );
}

#[test]
fn the_default_choice_is_the_default_theme() {
    assert_eq!(
        Theme::from_choice(ThemeChoice::default(), ColorDepth::TrueColor),
        Theme::default()
    );
}

#[test]
fn color_depth_reads_colorterm() {
    let env = |v: &'static str| move |name: &str| (name == "COLORTERM").then(|| v.to_string());
    assert_eq!(color_depth(env("truecolor")), ColorDepth::TrueColor);
    assert_eq!(color_depth(env("24bit")), ColorDepth::TrueColor);
    assert_eq!(color_depth(env("xterm")), ColorDepth::Ansi);
    assert_eq!(color_depth(|_| None), ColorDepth::Ansi);
}

fn general(text: &str) -> (General, String) {
    let config = rail_core::config::parse(text, Path::new("/c/config.toml")).unwrap();
    (config.general, text.to_string())
}

#[test]
fn from_config_resolves_the_scheme_and_accent() {
    let (g, text) = general("[general]\ntheme = \"dracula\"\naccent = \"#bd93f9\"\n");
    let choice = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap();
    assert_eq!(
        choice.scheme.map(|s| s.name),
        Some("Dracula"),
        "canonical name"
    );
    assert_eq!(choice.accent, Accent::Color(Color::Rgb(0xbd, 0x93, 0xf9)));
    let (g, text) = general("[general]\naccent = 3\n");
    let choice = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap();
    assert_eq!(
        choice,
        ThemeChoice {
            scheme: None,
            accent: Accent::Slot(3)
        }
    );
    let (g, text) = general("");
    assert_eq!(
        ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap(),
        ThemeChoice::default()
    );
}

#[test]
fn a_misspelt_theme_names_the_nearest_scheme_and_the_line() {
    let (g, text) = general("# top\n[general]\nicons = \"ascii\"\ntheme = \"Drakula\"\n");
    let err = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap_err();
    assert!(
        matches!(err, ThemeError::UnknownTheme { line: 4, .. }),
        "{err:?}"
    );
    let shown = err.to_string();
    assert!(
        shown.starts_with("/c/config.toml:4: no theme \"Drakula\""),
        "{shown}"
    );
    assert!(shown.ends_with("; did you mean \"Dracula\"?"), "{shown}");
    let (g, text) = general("[general]\ntheme = \"qqqqqqqqqqq\"\n");
    let shown = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml"))
        .unwrap_err()
        .to_string();
    assert!(!shown.contains("did you mean"), "{shown}");
}

#[test]
fn a_string_accent_that_is_a_slot_number_is_a_slot() {
    let (g, text) = general("[general]\naccent = \"5\"\n");
    let choice = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap();
    assert_eq!(choice.accent, Accent::Slot(5));
    let (g, text) = general("[general]\naccent = \"200\"\n");
    let err = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap_err();
    assert!(
        matches!(err, ThemeError::BadAccent { line: 2, .. }),
        "{err:?}"
    );
}

#[test]
fn a_bad_accent_colour_names_the_line() {
    let (g, text) = general("[general]\naccent = \"notacolour\"\n");
    let err = ThemeChoice::from_config(&g, &text, Path::new("/c/config.toml")).unwrap_err();
    assert!(
        matches!(err, ThemeError::BadAccent { line: 2, .. }),
        "{err:?}"
    );
    assert_eq!(g.accent, Some(AccentSpec::Color("notacolour".into())));
}
