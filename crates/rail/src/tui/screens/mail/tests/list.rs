//! The thread list in date groups through the mail screen: the group rows, the cursor and
//! paging over them, selection and opening across them, and the badge by scope.

use rail_core::config::Icons;
use rail_core::provider::Envelope;

use super::*;
use crate::tui::test_util::render_lines;

const HOUR_MS: i64 = 3_600_000;
const GROUPS: [&str; 4] = ["TODAY", "YESTERDAY", "THIS WEEK", "OLDER"];

/// `n` one-message inbox threads for `work`, `{prefix}t0`.., a minute apart from `start_ms`
/// (subjects `subject {prefix}m<i>`).
fn threads(prefix: &str, start_ms: i64, n: usize) -> Vec<(usize, Envelope)> {
    (0..n)
        .map(|i| {
            let ms = start_ms + i64::try_from(i).unwrap() * 60_000;
            let id = format!("{prefix}m{i}");
            (0, mail_envelope(&id, &format!("{prefix}t{i}"), ms))
        })
        .collect()
}

/// `work` with `today` threads on 2025-10-10 from 10:53 (`subject dm<i>`) and `yesterday`
/// threads on 2025-10-09 from 08:53 (`subject ym<i>`, `ym0` read), a minute apart, newest
/// first in the list; `home` has none. Started and settled with `icons`.
async fn grouped_with(today: usize, yesterday: usize, icons: Icons) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let mut older = threads("y", BASE_MS, yesterday);
    if let Some((_, oldest)) = older.first_mut() {
        oldest.is_read = true;
    }
    let mut envelopes = threads("d", BASE_MS + 26 * HOUR_MS, today);
    envelopes.extend(older);
    seed_store(
        &paths,
        &[("work", "w@example.com"), ("home", "h@example.com")],
        &envelopes,
    );
    let fake = Arc::new(FakeDaemon::default());
    *fake.statuses.lock().unwrap() = vec![
        status("work", "w@example.com", SyncPhase::Idle),
        status("home", "h@example.com", SyncPhase::Idle),
    ];
    let ctx = MailContext {
        icons,
        ..mail_ctx(&paths, fake.clone())
    };
    let mut screen = MailScreen::new(ctx, work_and_home());
    let last_theme = Arc::new(Mutex::new(None));
    let editor = Editor::default();
    let effects = screen.start();
    settle_capturing(&mut screen, effects, &last_theme, &editor).await;
    Fixture {
        _tmp: tmp,
        paths,
        fake,
        screen,
        last_theme,
        editor,
    }
}

/// `grouped_with` Unicode glyphs.
async fn grouped(today: usize, yesterday: usize) -> Fixture {
    grouped_with(today, yesterday, Icons::Unicode).await
}

/// The screen drawn at `width` x `height`, one string per row.
fn lines(f: &Fixture, width: u16, height: u16) -> Vec<String> {
    render_lines(width, height, |frame| {
        f.screen.view(frame, &Theme::default())
    })
}

/// The row between the header and the bar drawn in the selected style: the cursor's.
fn cursor_row(f: &Fixture, width: u16, height: u16) -> Option<u16> {
    let theme = Theme::default();
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| f.screen.view(frame, &theme)).unwrap();
    let buffer = terminal.backend().buffer();
    (1..height - 1).find(|&y| buffer[(8, y)].bg == theme.selected.bg.unwrap())
}

#[tokio::test]
async fn a_window_within_one_group_draws_one_group_row() {
    let f = grouped(0, 3).await;
    let rows = lines(&f, 90, 24);
    let groups: Vec<&str> = rows
        .iter()
        .map(|r| r.trim_end())
        .filter(|r| GROUPS.contains(r))
        .collect();
    assert_eq!(groups, ["YESTERDAY"]);
    assert_eq!(rows[1].trim_end(), "YESTERDAY");
    for (y, subject) in [(2, "subject ym2"), (3, "subject ym1"), (4, "subject ym0")] {
        assert!(rows[y].contains(subject), "{}", rows[y]);
    }
}

/// Review Focus 5 (the cursor).
#[tokio::test]
async fn j_from_the_last_of_today_lands_on_the_first_of_yesterday() {
    let mut f = grouped(2, 2).await;
    press(&mut f, KeyCode::Char('j')).await;
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject dm0"),
        "the last of TODAY"
    );
    press(&mut f, KeyCode::Char('j')).await;
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject ym1"),
        "one key over the group row"
    );
    let rows = lines(&f, 90, 24);
    assert_eq!(rows[4].trim_end(), "YESTERDAY");
    assert!(rows[5].contains("subject ym1"), "{}", rows[5]);
    assert_eq!(cursor_row(&f, 90, 24), Some(5));
    press(&mut f, KeyCode::Char('k')).await;
    assert_eq!(selected(&f).as_deref(), Some("subject dm0"));
    assert_eq!(cursor_row(&f, 90, 24), Some(3));
}

/// Review Focus 5 (paging).
#[tokio::test]
async fn a_page_down_moves_by_the_threads_that_fit() {
    let mut f = grouped(0, 40).await;
    let rows = lines(&f, 90, 24);
    assert_eq!(rows[1].trim_end(), "YESTERDAY");
    assert_eq!(
        f.screen.height(),
        21,
        "22 list rows, one of them the group row"
    );
    press(&mut f, KeyCode::PageDown).await;
    assert_eq!(f.screen.list.selected_index(), 21);
    let rows = lines(&f, 90, 24);
    assert_eq!(
        rows[1].trim_end(),
        "YESTERDAY",
        "the group's row stays on top"
    );
    assert!(rows[22].contains("subject ym18"), "{}", rows[22]);
    assert_eq!(
        cursor_row(&f, 90, 24),
        Some(22),
        "the cursor on the last list row"
    );
}
