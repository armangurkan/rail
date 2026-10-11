//! `:theme` and `:accent`: preview on every move, Esc restores, Enter keeps and writes config,
//! the non-truecolor path, a failed write.

use ratatui::style::Color;

use super::*;
use crate::tui::modals::Target;
use crate::tui::theme::choice::{Accent, ColorDepth, ThemeChoice};
use crate::tui::theme::scheme::scheme;

/// Open the palette and run `target` as Enter would.
async fn run_target(f: &mut Fixture, target: Target) {
    let effects = f.screen.run_palette(target);
    settle_capturing(&mut f.screen, effects, &f.last_theme).await;
}

/// Type `text` into the open picker.
async fn type_text(f: &mut Fixture, text: &str) {
    for c in text.chars() {
        press(f, KeyCode::Char(c)).await;
    }
}

fn applied_theme(f: &Fixture) -> Option<Theme> {
    *f.last_theme.lock().unwrap()
}

/// Write `config.toml` as the user would have it.
fn write_config(f: &Fixture, text: &str) {
    let path = f.paths.config_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn config_text(f: &Fixture) -> String {
    std::fs::read_to_string(f.paths.config_file()).unwrap_or_default()
}

#[tokio::test]
async fn moving_in_theme_previews_and_enter_keeps_and_saves() {
    let mut f = fixture(3).await;
    run_target(&mut f, Target::Theme).await;
    assert!(matches!(f.screen.modal, Some(Modal::Picker(_))));
    type_text(&mut f, "dracula").await;
    let previewed = applied_theme(&f).expect("a preview was applied");
    assert_eq!(previewed.background, Some(Color::Rgb(0x28, 0x2a, 0x36)));
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.modal.is_none());
    assert_eq!(
        f.screen.themes.current.scheme.map(|s| s.name),
        Some("Dracula")
    );
    assert!(
        config_text(&f).contains("theme = \"Dracula\""),
        "{}",
        config_text(&f)
    );
    assert!(
        f.screen.status_line().contains("theme: Dracula"),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn esc_restores_the_theme_from_before_the_picker_opened() {
    let mut f = fixture(3).await;
    f.screen.themes.current = ThemeChoice {
        scheme: scheme("Nord"),
        accent: Accent::Slot(3),
    };
    run_target(&mut f, Target::Theme).await;
    type_text(&mut f, "dracula").await;
    press(&mut f, KeyCode::Down).await;
    press(&mut f, KeyCode::Up).await;
    press(&mut f, KeyCode::Esc).await;
    let restored = applied_theme(&f).expect("the restore was applied");
    let nord = Theme::from_choice(f.screen.themes.current, ColorDepth::TrueColor);
    assert_eq!(restored, nord, "Nord with accent slot 3 is back");
    assert_eq!(f.screen.themes.current.accent, Accent::Slot(3));
    assert!(!config_text(&f).contains("theme"), "nothing was written");
}

#[tokio::test]
async fn rail_default_removes_the_key_and_accent_writes_a_slot() {
    let mut f = fixture(3).await;
    write_config(&f, "[general]\ntheme = \"Nord\"\n");
    run_target(&mut f, Target::Theme).await;
    type_text(&mut f, "rail default").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(!config_text(&f).contains("theme ="), "{}", config_text(&f));
    assert!(f.screen.status_line().contains("theme: Rail default"));
    run_target(&mut f, Target::Accent).await;
    press_n(&mut f, KeyCode::Down, 6).await; // scheme default, 0..4, 5
    press(&mut f, KeyCode::Enter).await;
    assert!(
        config_text(&f).contains("accent = 5"),
        "{}",
        config_text(&f)
    );
    assert!(
        f.screen.status_line().contains("accent: 5 magenta"),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.themes.current.accent, Accent::Slot(5));
}

#[tokio::test]
async fn an_ansi_terminal_keeps_the_default_theme_and_still_saves() {
    let nord = ThemeChoice {
        scheme: scheme("Nord"),
        accent: Accent::SchemeDefault,
    };
    let mut f = fixture_with(3, ColorDepth::Ansi, nord).await;
    assert!(
        f.screen
            .status_line()
            .contains("theme needs a truecolor terminal")
    );
    run_target(&mut f, Target::Theme).await;
    type_text(&mut f, "dracula").await;
    assert_eq!(applied_theme(&f), None, "no preview without truecolor");
    press(&mut f, KeyCode::Enter).await;
    assert!(config_text(&f).contains("theme = \"Dracula\""));
    assert_eq!(
        applied_theme(&f).map(|t| t.background),
        Some(None),
        "still the default look"
    );
}

#[tokio::test]
async fn a_failed_config_write_keeps_the_theme_and_reports() {
    let mut f = fixture(3).await;
    write_config(&f, "[general]\n");
    // `rename` replaces a read-only file, so the directory is what must refuse the write.
    let dir = f.paths.config_file().parent().unwrap().to_path_buf();
    let writable = std::fs::metadata(&dir).unwrap().permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&dir, read_only).unwrap();
    if std::fs::write(dir.join("probe"), "x").is_ok() {
        eprintln!("skipped: this user can write a read-only directory");
        std::fs::set_permissions(&dir, writable).unwrap();
        return;
    }
    run_target(&mut f, Target::Theme).await;
    type_text(&mut f, "dracula").await;
    press(&mut f, KeyCode::Enter).await;
    std::fs::set_permissions(&dir, writable).unwrap();
    assert_eq!(
        f.screen.themes.current.scheme.map(|s| s.name),
        Some("Dracula"),
        "kept for the session"
    );
    assert!(
        f.screen.status_line().contains("Could not save the theme"),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.error_count(), 1);
}

#[tokio::test]
async fn the_help_and_palette_list_both_commands() {
    let f = fixture(3).await;
    let entries = f.screen.palette_entries();
    assert!(
        entries
            .iter()
            .any(|e| e.target == Target::Theme && e.text == "Change the colour theme")
    );
    assert!(
        entries
            .iter()
            .any(|e| e.target == Target::Accent && e.text == "Change the accent colour")
    );
}
