//! The thread list in date groups through the mail screen: the group rows, the cursor and
//! paging over them, selection and opening across them, and the badge by scope.

use rail_core::config::Icons;
use rail_core::provider::Envelope;

use super::*;
use crate::tui::test_util::render_lines;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;
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
    let mut older = threads("y", BASE_MS, yesterday);
    if let Some((_, oldest)) = older.first_mut() {
        oldest.is_read = true;
    }
    let mut envelopes = threads("d", BASE_MS + 26 * HOUR_MS, today);
    envelopes.extend(older);
    seeded(&envelopes, icons).await
}

/// `work` with `envelopes` (`home` has none), started and settled with `icons`.
async fn seeded(envelopes: &[(usize, Envelope)], icons: Icons) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    seed_store(
        &paths,
        &[("work", "w@example.com"), ("home", "h@example.com")],
        envelopes,
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
    assert!(
        rows[2].contains("subject ym20"),
        "held at the last full screen: {}",
        rows[2]
    );
    assert!(rows[22].contains("subject ym0"), "{}", rows[22]);
    assert_eq!(cursor_row(&f, 90, 24), Some(4), "the cursor on ym18");
}

/// Spec §5 (C1): a page over a screen with three group rows lands on the thread after the last
/// one shown, and the next screen starts with it: no thread is skipped.
#[tokio::test]
async fn a_page_down_over_three_group_rows_starts_with_the_thread_after_the_last_shown() {
    let mut envelopes = threads("o", BASE_MS - 10 * DAY_MS, 30);
    envelopes.extend(threads("y", BASE_MS, 5));
    envelopes.extend(threads("d", BASE_MS + 26 * HOUR_MS, 5));
    let mut f = seeded(&envelopes, Icons::Unicode).await;
    let before = lines(&f, 90, 24);
    assert_eq!(before[1].trim_end(), "TODAY");
    assert!(
        before[22].contains("subject om21"),
        "the last shown: {}",
        before[22]
    );
    press(&mut f, KeyCode::PageDown).await;
    assert_eq!(selected(&f).as_deref(), Some("subject om20"));
    let after = lines(&f, 90, 24);
    assert_eq!(after[1].trim_end(), "OLDER");
    assert!(
        after[2].contains("subject om20"),
        "the thread after the last one shown: {}",
        after[2]
    );
    assert_eq!(cursor_row(&f, 90, 24), Some(2));
    press(&mut f, KeyCode::PageUp).await;
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject dm4"),
        "back to the top"
    );
}

/// The final review's finding 1: the cursor on the bottom row and a next screen with more group
/// rows (so fewer threads) than this one. The next screen still starts with the thread after
/// the last one shown, and the cursor clamps to its last thread.
#[tokio::test]
async fn a_page_down_from_the_bottom_row_starts_with_the_thread_after_the_last_shown() {
    let mut envelopes = threads("o", BASE_MS - 10 * DAY_MS, 30);
    envelopes.extend(threads("w", BASE_MS - 2 * DAY_MS, 2));
    envelopes.extend(threads("y", BASE_MS, 2));
    envelopes.extend(threads("d", BASE_MS + 26 * HOUR_MS, 21));
    let mut f = seeded(&envelopes, Icons::Unicode).await;
    let before = lines(&f, 90, 24);
    assert_eq!(before[1].trim_end(), "TODAY");
    assert!(
        before[22].contains("subject dm0"),
        "the last shown: {}",
        before[22]
    );
    for _ in 0..20 {
        press(&mut f, KeyCode::Char('j')).await;
    }
    assert_eq!(
        cursor_row(&f, 90, 24),
        Some(22),
        "the cursor on the bottom row"
    );
    press(&mut f, KeyCode::PageDown).await;
    let after = lines(&f, 90, 24);
    assert_eq!(after[1].trim_end(), "YESTERDAY", "{after:?}");
    assert!(
        after[2].contains("subject ym1"),
        "the thread after the last one shown: {}",
        after[2]
    );
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject om15"),
        "the screen's last thread"
    );
    assert_eq!(cursor_row(&f, 90, 24), Some(22));
}

/// The mirror: the cursor on the top row and a screen above with more group rows than this one.
/// The previous screen ends with the thread before the first one shown.
#[tokio::test]
async fn a_page_up_from_the_top_row_ends_with_the_thread_before_the_first_shown() {
    let mut envelopes = threads("o", BASE_MS - 10 * DAY_MS, 40);
    envelopes.extend(threads("w", BASE_MS - 2 * DAY_MS, 2));
    envelopes.extend(threads("y", BASE_MS, 2));
    envelopes.extend(threads("d", BASE_MS + 26 * HOUR_MS, 2));
    let mut f = seeded(&envelopes, Icons::Unicode).await;
    let _ = lines(&f, 90, 24);
    press(&mut f, KeyCode::End).await;
    for _ in 0..20 {
        press(&mut f, KeyCode::Char('k')).await;
    }
    let before = lines(&f, 90, 24);
    assert_eq!(before[1].trim_end(), "OLDER");
    assert!(
        before[2].contains("subject om20"),
        "the first shown: {}",
        before[2]
    );
    assert_eq!(cursor_row(&f, 90, 24), Some(2), "the cursor on the top row");
    press(&mut f, KeyCode::PageUp).await;
    let after = lines(&f, 90, 24);
    assert!(
        after[22].contains("subject om21"),
        "the thread before the first one shown: {after:?}"
    );
    assert_eq!(after[1].trim_end(), "THIS WEEK", "{after:?}");
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject wm0"),
        "the cursor keeps its row"
    );
    assert_eq!(cursor_row(&f, 90, 24), Some(2));
}

/// Review Focus 5 (selection and opening).
#[tokio::test]
async fn space_selects_across_a_group_row_and_enter_opens_the_thread_under_the_cursor() {
    let mut f = grouped(2, 2).await;
    let ids = thread_ids(&f);
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    assert_eq!(
        selected(&f).as_deref(),
        Some("subject ym1"),
        "Space moved over the group row"
    );
    press(&mut f, KeyCode::Char(' ')).await;
    let selection = &f.screen.actions.selection;
    assert_eq!(selection.len(), 2);
    assert!(selection.contains(ids[1]) && selection.contains(ids[2]));
    let rows = lines(&f, 90, 24);
    assert!(rows[3].starts_with("▸  W  Sender"), "{}", rows[3]);
    assert_eq!(rows[4].trim_end(), "YESTERDAY");
    assert!(rows[5].starts_with("▸  W  Sender"), "{}", rows[5]);
    assert!(
        rows[6].starts_with("·  W  Sender"),
        "the read thread: {}",
        rows[6]
    );
    press(&mut f, KeyCode::Char('k')).await;
    press(&mut f, KeyCode::Enter).await;
    let reader = f.screen.reader.as_ref().expect("a thread opened");
    assert_eq!(reader.thread_id(), ids[2], "the thread under the cursor");
    let rows = lines(&f, 150, 40);
    assert!(
        rows[4].contains("│TODAY"),
        "the list beside the reader: {}",
        rows[4]
    );
    assert!(
        rows[4].contains("subject ym1"),
        "the reader's title: {}",
        rows[4]
    );
}

/// Review Focus 4 (through the screen).
#[tokio::test]
async fn home_and_end_show_their_group_rows_and_the_cursor() {
    let mut f = grouped(10, 10).await;
    let _ = lines(&f, 90, 12);
    press(&mut f, KeyCode::End).await;
    assert_eq!(selected(&f).as_deref(), Some("subject ym0"));
    let rows = lines(&f, 90, 12);
    assert_eq!(
        rows[1].trim_end(),
        "YESTERDAY",
        "the group of what is on screen"
    );
    assert!(rows[10].contains("subject ym0"), "{}", rows[10]);
    assert_eq!(
        cursor_row(&f, 90, 12),
        Some(10),
        "the cursor on the last list row"
    );
    press(&mut f, KeyCode::Home).await;
    assert_eq!(selected(&f).as_deref(), Some("subject dm9"));
    let rows = lines(&f, 90, 12);
    assert_eq!(rows[1].trim_end(), "TODAY");
    assert_eq!(cursor_row(&f, 90, 12), Some(2));
}

/// The final review's I1: `End` from a screen with four group rows leaves the pane drawing
/// threads above the remembered first row; a re-read must keep them in the window.
#[tokio::test]
async fn a_re_read_after_end_keeps_the_drawn_threads_and_fills_the_pane() {
    let mut envelopes = threads("o", BASE_MS - 10 * DAY_MS, 10);
    envelopes.extend(threads("w", BASE_MS - 2 * DAY_MS, 1));
    envelopes.extend(threads("y", BASE_MS, 1));
    envelopes.extend(threads("d", BASE_MS + 26 * HOUR_MS, 1));
    let mut f = seeded(&envelopes, Icons::Unicode).await;
    let home = lines(&f, 90, 12);
    for group in GROUPS {
        assert!(home.iter().any(|r| r.trim_end() == group), "{home:?}");
    }
    press(&mut f, KeyCode::End).await;
    assert_eq!(selected(&f).as_deref(), Some("subject om0"));
    let before = lines(&f, 90, 12);
    assert_eq!(before[1].trim_end(), "OLDER", "{before:?}");
    assert!(before[2].contains("subject om8"), "{before:?}");
    assert!(before[10].contains("subject om0"), "{before:?}");
    store_changed(&mut f).await;
    let after = lines(&f, 90, 12);
    assert_eq!(
        after[1..11],
        before[1..11],
        "the same threads on screen after the re-read"
    );
    assert!(
        after[1..11].iter().all(|r| !r.trim().is_empty()),
        "no blank list row: {after:?}"
    );
}

/// Spec §5 (C4): the groups are worked out once per list read and per day, not per frame.
#[tokio::test]
async fn the_groups_are_worked_out_once_per_read_and_per_day() {
    let mut f = grouped(2, 2).await;
    let _ = lines(&f, 90, 24);
    let _ = lines(&f, 90, 24);
    assert_eq!(f.screen.list_groups.builds(), 1, "two frames, one read");
    store_changed(&mut f).await;
    let _ = lines(&f, 90, 24);
    assert_eq!(
        f.screen.list_groups.builds(),
        2,
        "a list read works them out again"
    );
    f.screen
        .ctx
        .clock
        .set_now(crate::tui::test_util::NOW + 86_400);
    let rows = lines(&f, 90, 24);
    assert_eq!(f.screen.list_groups.builds(), 3, "so does a new day");
    assert_eq!(
        rows[1].trim_end(),
        "YESTERDAY",
        "today's threads are yesterday's now"
    );
}

#[tokio::test]
async fn the_badge_shows_in_merged_mailboxes_and_not_in_an_accounts_inbox() {
    let mut f = grouped(2, 0).await;
    let first = |f: &Fixture| lines(f, 90, 24)[2].clone();
    assert!(first(&f).starts_with("●  W  Sender"), "{}", first(&f));
    let theme = Theme::default();
    let backend = ratatui::backend::TestBackend::new(90, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| f.screen.view(frame, &theme)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert_eq!(
        buffer[(3, 3)].symbol(),
        "W",
        "the second thread, off the cursor row"
    );
    assert_eq!(
        buffer[(3, 3)].bg,
        ratatui::style::Color::Cyan,
        "work's colour"
    );
    assert_eq!(buffer[(3, 3)].fg, ratatui::style::Color::Black);
    press(&mut f, KeyCode::Char('1')).await;
    assert!(
        first(&f).starts_with("● Sender"),
        "no badge in work's inbox: {}",
        first(&f)
    );
    press(&mut f, KeyCode::Char('0')).await;
    assert!(first(&f).starts_with("●  W  Sender"), "{}", first(&f));
}

/// `(more match)` belongs to the capped select-all that set it, so every other way the
/// selection changes takes it off again: it would otherwise claim that one hand-picked row is
/// the first of ten thousand.
#[tokio::test]
async fn the_capped_select_all_note_goes_when_the_selection_changes_another_way() {
    let mut f = fixture(3).await;
    f.screen.bulk.more = true;
    press(&mut f, KeyCode::Char(' ')).await;
    assert_eq!(f.screen.actions.selection.len(), 1);
    assert!(!f.screen.bulk.more, "`Space` picked one row");
    assert!(
        !f.screen.header_left().contains("more match"),
        "`Space` takes `(more match)` off the header"
    );

    f.screen.bulk.more = true;
    press(&mut f, KeyCode::Char('e')).await;
    assert!(
        f.screen.actions.selection.is_empty(),
        "the action took the selection"
    );
    assert!(!f.screen.bulk.more, "and the note with it");

    f.screen.bulk.more = true;
    press(&mut f, KeyCode::Esc).await;
    assert!(!f.screen.bulk.more, "`Esc` clears both");
}

#[tokio::test]
async fn the_grouped_list_in_ascii() {
    let f = grouped_with(2, 2, Icons::Ascii).await;
    insta::assert_snapshot!(
        "grouped_list_ascii_90x24",
        crate::tui::test_util::snapshot(90, 24, |frame| {
            f.screen.view(frame, &Theme::default())
        })
    );
}
