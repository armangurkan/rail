//! `:theme` and `:accent`: preview on every move, Esc restores, Enter keeps and writes config,
//! the non-truecolor path, a failed write.

use ratatui::style::Color;

use super::*;
use crate::tui::modals::Target;
use crate::tui::screens::mail::themes::NEEDS_TRUECOLOR;
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

/// The screen drawn at 90x24.
fn drawn(f: &Fixture) -> String {
    crate::tui::test_util::snapshot(90, 24, |frame| f.screen.view(frame, &Theme::default()))
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
    let hint = drawn(&f);
    assert!(hint.contains("no preview without truecolor"), "{hint}");
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

#[tokio::test]
async fn without_truecolor_only_the_theme_picker_says_it_needs_truecolor() {
    let mut f = fixture_with_depth(3, ColorDepth::Ansi).await;
    assert!(!f.screen.status_line().contains(NEEDS_TRUECOLOR));
    run_target(&mut f, Target::Accent).await;
    assert!(
        !f.screen.status_line().contains(NEEDS_TRUECOLOR),
        "{}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Down).await;
    assert!(
        applied_theme(&f).is_some(),
        "slot accents preview on any terminal"
    );
    press(&mut f, KeyCode::Esc).await;
    run_target(&mut f, Target::Theme).await;
    assert!(f.screen.status_line().contains(NEEDS_TRUECOLOR));
}

#[tokio::test]
async fn a_thread_leaving_the_store_under_a_previewing_picker_restores_the_theme() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Enter).await;
    let thread_id = f.screen.reader().expect("the reader is open").thread_id();
    run_target(&mut f, Target::Theme).await;
    type_text(&mut f, "dracula").await;
    assert_eq!(
        applied_theme(&f).and_then(|t| t.background),
        Some(Color::Rgb(0x28, 0x2a, 0x36)),
        "Dracula is previewed"
    );
    let gone = MailEvent::Thread {
        seq: f.screen.latest_thread,
        thread_id,
        result: Ok(None),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(gone)));
    settle_capturing(&mut f.screen, effects, &f.last_theme).await;
    assert!(f.screen.reader().is_none() && f.screen.modal.is_none());
    let kept = Theme::from_choice(f.screen.themes.current, ColorDepth::TrueColor);
    assert_eq!(applied_theme(&f), Some(kept), "the kept look is back");
    assert_eq!(f.screen.themes.before, None);
}

#[cfg(unix)]
#[test]
fn saving_through_a_symlinked_config_keeps_the_link() {
    use crate::tui::screens::mail::themes::{Saved, write_general};
    let real_dir = tempfile::tempdir().unwrap();
    let link_dir = tempfile::tempdir().unwrap();
    let real = real_dir.path().join("config.toml");
    let link = link_dir.path().join("config.toml");
    std::fs::write(&real, "[general]\neditor = \"vi\"\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    write_general(&link, "theme", Saved::Text(Some("Dracula".into()))).unwrap();

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let text = std::fs::read_to_string(&real).unwrap();
    assert!(text.contains("theme = \"Dracula\""), "{text}");
    assert!(text.contains("editor = \"vi\""), "{text}");
}

#[test]
fn two_saves_at_once_both_land_in_the_file() {
    use crate::tui::screens::mail::themes::{Saved, write_general};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[general]\n").unwrap();
    for _ in 0..20 {
        std::fs::write(&path, "[general]\n").unwrap();
        let (a, b) = (path.clone(), path.clone());
        let theme = std::thread::spawn(move || {
            write_general(&a, "theme", Saved::Text(Some("Dracula".into())))
        });
        let accent =
            std::thread::spawn(move || write_general(&b, "accent", Saved::Number(Some(3))));
        theme.join().unwrap().unwrap();
        accent.join().unwrap().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("theme = \"Dracula\""), "{text}");
        assert!(text.contains("accent = 3"), "{text}");
    }
}
