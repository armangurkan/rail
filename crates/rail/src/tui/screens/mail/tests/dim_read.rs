use super::*;
use rail_core::config::DimRead;

async fn cycle(f: &mut Fixture) {
    press(f, KeyCode::Char(':')).await;
    for c in "dim-read".chars() {
        press(f, KeyCode::Char(c)).await;
    }
    press(f, KeyCode::Enter).await;
}

#[tokio::test]
async fn dim_read_palette_cycles_and_saves_all_modes_without_losing_comments() {
    let mut f = fixture(3).await;
    let path = f.paths.config_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[general]\neditor = \"vi\" # mine\n").unwrap();
    for expected in [DimRead::Subject, DimRead::All, DimRead::Dot] {
        cycle(&mut f).await;
        assert!(f.screen.modal.is_none());
        assert_eq!(f.screen.ctx.dim_read, expected);
        assert!(
            f.screen
                .status_line()
                .contains(&format!("dim read: {}", expected.as_str()))
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("editor = \"vi\" # mine"));
        assert_eq!(
            rail_core::config::parse(&text, &path)
                .unwrap()
                .general
                .dim_read,
            expected
        );
        assert_eq!(
            text.contains("dim_read"),
            expected != DimRead::Dot,
            "default removes the key"
        );
    }
}

#[tokio::test]
async fn dim_read_save_failure_keeps_the_session_choice_and_reports_the_error() {
    let mut f = fixture(3).await;
    crate::tui::test_util::block_config(&f.paths);
    cycle(&mut f).await;
    assert_eq!(f.screen.ctx.dim_read, DimRead::Subject);
    assert!(
        f.screen
            .status_line()
            .contains("Could not save the read dimming setting")
    );
    assert_eq!(f.screen.error_count(), 1);
}
