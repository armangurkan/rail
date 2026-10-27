//! Saving and opening a thread's attachments from the reader: the list with its cursor and its
//! message headings, `s`, `S` and `w`, the once-a-session open question, the status line each
//! outcome gets, the modal's snapshots, and the copies removed when the reader closes.

use std::path::PathBuf;

use rail_core::config::{AccountKind, Icons};
use rail_core::error::ErrorClass;
use rail_core::ops::{
    AttachmentOpenResponse, AttachmentOutcome, AttachmentResult, AttachmentSaveResponse, ErrorCode,
};
use rail_core::provider::{AttachmentMeta, Body};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use url::Url;

use super::*;
use crate::tui::modals::{AttachmentRow, AttachmentTarget};
use crate::tui::test_util::{recording_opener, render_lines, snapshot};

mod keys;
mod mismatch;
mod outcomes;

/// Fixture **F**'s parts: `(remote message id, part id, filename, size, mime type)`.
const PARTS: &[(&str, &str, &str, i64, &str)] = &[
    ("m1", "0.1", "plan.pdf", 2048, "application/pdf"),
    ("m1", "0.2", "logo.png", 512, "image/png"),
    (
        "m2",
        "0.1",
        "Q4 numbers.xlsx",
        18_800,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
];

/// The hint on the modal's last inside row.
const HINT: &str = "Enter or o opens · s saves · S saves all · j/k move · Esc closes";

/// The open question's three sentences (§6 slot 1).
const OPEN_LINES: [&str; 3] = [
    "Rail writes this attachment to a private temporary file and hands it to your system opener.",
    "Which program runs is your system's choice, and the file came from the sender, not from Rail.",
    "Rail asks once per session; the files are removed when you close the reader.",
];

/// One row inside the box: marker 2 · name 38 · 1 · type 24 · 1 · size 8, right-aligned, over
/// the inner width of 74.
fn part_row(marker: &str, name: &str, mime: &str, size: &str) -> String {
    format!("{marker}{name:<38} {mime:<24} {size:>8}")
}

/// That row with the box's borders either side, as a frame draws it.
fn boxed_row(marker: &str, name: &str, mime: &str, size: &str) -> String {
    format!("│{}│", part_row(marker, name, mime, size))
}

/// Fixture **F**: account `work`, one thread of two messages. m1 holds `plan.pdf` (part `0.1`)
/// and `logo.png` (part `0.2`), m2 `Q4 numbers.xlsx` (part `0.1`); the reader is open on the
/// thread and the opener records.
async fn attachment_fixture() -> (Fixture, Arc<Mutex<Vec<Url>>>) {
    attachment_fixture_with(PARTS, &[]).await
}

/// `attachment_fixture` over `parts`, with `extra_messages` seeded newer and holding no
/// attachments.
async fn attachment_fixture_with(
    parts: &[(&str, &str, &str, i64, &str)],
    extra_messages: &[&str],
) -> (Fixture, Arc<Mutex<Vec<Url>>>) {
    attachment_fixture_icons(parts, extra_messages, Icons::Unicode).await
}

/// `attachment_fixture_with` on a terminal drawing `icons`: the thread's messages are the ones
/// `parts` names (oldest first) and then `extra_messages`, each with its body in the store, the
/// reader open, the fake's open directory set and no call recorded yet.
async fn attachment_fixture_icons(
    parts: &[(&str, &str, &str, i64, &str)],
    extra_messages: &[&str],
    icons: Icons,
) -> (Fixture, Arc<Mutex<Vec<Url>>>) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let mut ids: Vec<String> = Vec::new();
    for part in parts {
        if !ids.iter().any(|id| id.as_str() == part.0) {
            ids.push(part.0.to_string());
        }
    }
    ids.extend(extra_messages.iter().map(|id| (*id).to_string()));
    let envelopes: Vec<(usize, rail_core::provider::Envelope)> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let ms = BASE_MS + i64::try_from(index).unwrap() * 60_000;
            (0, mail_envelope(id, "t1", ms))
        })
        .collect();
    let mut store = seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let bodies: Vec<(String, Body)> = ids
        .iter()
        .map(|id| {
            let attachments: Vec<AttachmentMeta> = parts
                .iter()
                .filter(|part| part.0 == id.as_str())
                .map(|&(_, part_id, filename, size, mime_type)| AttachmentMeta {
                    part_id: part_id.to_string(),
                    filename: filename.to_string(),
                    mime_type: mime_type.to_string(),
                    size,
                    plain_file: true,
                })
                .collect();
            let body = Body {
                text: format!("the body of {id}"),
                attachments,
                ..Body::default()
            };
            (id.clone(), body)
        })
        .collect();
    store
        .apply(
            &Batch {
                bodies,
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let fake = Arc::new(FakeDaemon::default());
    *fake.statuses.lock().unwrap() = vec![status("work", "w@example.com", SyncPhase::Idle)];
    let (opener, urls) = recording_opener();
    let ctx = MailContext {
        opener,
        icons,
        ..mail_ctx(&paths, fake.clone())
    };
    let mut screen = MailScreen::new(ctx, work_and_home()[..1].to_vec());
    let last_theme = Arc::new(Mutex::new(None));
    let editor = Editor::default();
    let effects = screen.start();
    settle_capturing(&mut screen, effects, &last_theme, &editor).await;
    let mut f = Fixture {
        _tmp: tmp,
        paths,
        fake,
        screen,
        last_theme,
        editor,
    };
    press(&mut f, KeyCode::Enter).await;
    *f.fake.open_dir.lock().unwrap() = Some(f.paths.attachments_dir());
    f.fake.calls.lock().unwrap().clear();
    (f, urls)
}

/// `attachment_fixture` with the session's open question already answered yes, and nothing
/// recorded since.
async fn confirmed_fixture() -> (Fixture, Arc<Mutex<Vec<Url>>>) {
    let (mut f, opened) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('y')).await;
    f.fake.calls.lock().unwrap().clear();
    opened.lock().unwrap().clear();
    (f, opened)
}

/// Where `mail_ctx` points `attachment_dir`.
fn saved_dir(f: &Fixture) -> PathBuf {
    f.paths.cache_dir.join("saved")
}

fn saved_text(f: &Fixture) -> String {
    saved_dir(f).display().to_string()
}

fn draw(f: &Fixture, width: u16, height: u16) -> String {
    snapshot(width, height, |frame| {
        f.screen.view(frame, &Theme::default());
    })
}

fn rows(f: &Fixture, width: u16, height: u16) -> Vec<String> {
    render_lines(width, height, |frame| {
        f.screen.view(frame, &Theme::default());
    })
}

/// The column the box's own border starts in on its top row (the sidebar may draw a box of its
/// own further left).
fn box_left(top: &str) -> usize {
    let cells: Vec<char> = top.chars().collect();
    let title: Vec<char> = "┌ Attachments ".chars().collect();
    (0..cells.len())
        .find(|&at| cells[at..].starts_with(title.as_slice()))
        .expect("the box's top border")
}

/// The rows inside the attachments box that carry the cursor marker, cut from the box's left
/// border: the reader drawn behind the box marks a collapsed card with the same glyph at the
/// frame's first column, outside the box.
fn marked_in_box(rows: &[String]) -> Vec<String> {
    let top = rows
        .iter()
        .position(|row| row.contains("┌ Attachments "))
        .expect("the box's top border");
    let bottom = rows[top + 1..]
        .iter()
        .position(|row| row.contains('└'))
        .map(|at| top + 1 + at)
        .expect("the box's bottom border");
    let left = box_left(&rows[top]);
    rows[top + 1..bottom]
        .iter()
        .map(|row| row.chars().skip(left).collect::<String>())
        .filter(|inside| inside.contains("▸ "))
        .collect()
}

fn calls(f: &Fixture) -> Vec<FakeCall> {
    f.fake.calls.lock().unwrap().clone()
}

/// The save the screen sent, as a `FakeCall`.
fn save_call(account: &str, message_id: &str, part_ids: &[&str], dir: &str) -> FakeCall {
    FakeCall::AttachmentSave {
        account: account.to_string(),
        message_id: message_id.to_string(),
        part_ids: part_ids.iter().map(|p| (*p).to_string()).collect(),
        dir: dir.to_string(),
    }
}

/// One `Saved` result for `part`, written as `path`.
fn saved_result(part: &str, path: &std::path::Path, size: i64) -> AttachmentResult {
    AttachmentResult {
        part_id: part.to_string(),
        outcome: AttachmentOutcome::Saved {
            path: path.display().to_string(),
            size,
        },
    }
}

/// One `Failed` result for `part`.
fn failure(part: &str, error: &OpError) -> AttachmentResult {
    AttachmentResult {
        part_id: part.to_string(),
        outcome: AttachmentOutcome::Failed(error.clone()),
    }
}

/// The answer the fake gives the next `attachment.save`.
fn answer_save(f: &Fixture, results: Vec<AttachmentResult>) {
    *f.fake.save_answer.lock().unwrap() = Some(Ok(AttachmentSaveResponse { results }));
}

/// The answer the fake gives the next `attachment.open`.
fn answer_open(f: &Fixture, result: Result<AttachmentOpenResponse, OpError>) {
    *f.fake.open_answer.lock().unwrap() = Some(result);
}

fn permanent(code: ErrorCode, message: &str) -> OpError {
    OpError::new(code, ErrorClass::Permanent, message)
}

/// Move the reader's cursor, one line at a time, onto a line of the message `remote`.
async fn cursor_onto_message(f: &mut Fixture, remote: &str) {
    for _ in 0..200 {
        let found = f
            .screen
            .reader
            .as_ref()
            .and_then(|r| r.message_at_cursor())
            .is_some_and(|m| m.remote_id == remote);
        if found {
            return;
        }
        press(f, KeyCode::Char('j')).await;
    }
    panic!("no line of {remote} under the cursor");
}

/// Move the reader's cursor onto a drawn line that belongs to no message (a rule, a gap or the
/// left-out note), so `message_at_cursor()` is `None`.
async fn cursor_onto_no_message(f: &mut Fixture) {
    for _ in 0..200 {
        let nowhere = f
            .screen
            .reader
            .as_ref()
            .is_some_and(|r| r.cursor_line().is_some() && r.message_at_cursor().is_none());
        if nowhere {
            return;
        }
        press(f, KeyCode::Char('j')).await;
    }
    panic!("no line of no message under the cursor");
}

/// Move the modal's cursor down until `name`'s row carries the marker.
async fn highlight(f: &mut Fixture, name: &str) {
    for _ in 0..40 {
        let marked = rows(f, 90, 24)
            .iter()
            .any(|row| row.contains("▸ ") && row.contains(name));
        if marked {
            return;
        }
        press(f, KeyCode::Char('j')).await;
    }
    panic!("{name} never took the marker");
}

/// AC-1: `a` lists every message's attachments under its heading, with the cursor on the first
/// part and nothing sent.
#[tokio::test]
async fn a_lists_every_message_s_attachments_with_a_cursor() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    assert!(
        matches!(f.screen.modal, Some(Modal::Attachments(_))),
        "the attachment list is open"
    );
    let text = draw(&f, 90, 24);
    assert!(text.contains("┌ Attachments "), "{text}");
    assert!(text.contains(&format!("│{:<74}│", "MESSAGE 1")), "{text}");
    assert!(
        text.contains(&boxed_row("▸ ", "plan.pdf", "application/pdf", "2.0 KB")),
        "{text}"
    );
    assert!(
        text.contains(&boxed_row("  ", "logo.png", "image/png", "512 B")),
        "{text}"
    );
    assert!(text.contains(&format!("│{:<74}│", "MESSAGE 2")), "{text}");
    assert!(text.contains("│  Q4 numbers.xlsx"), "{text}");
    assert!(text.contains(HINT), "{text}");
    assert!(calls(&f).is_empty(), "{:?}", calls(&f));
}

/// AC-2: `j` and `k` move the cursor over the parts and step over the headings.
#[tokio::test]
async fn j_and_k_move_the_cursor_over_parts_and_skip_headings() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Char('k')).await;
    let marked = marked_in_box(&rows(&f, 90, 24));
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(marked[0].contains("logo.png"), "{marked:?}");
    press(&mut f, KeyCode::Char('j')).await;
    let marked = marked_in_box(&rows(&f, 90, 24));
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(
        marked[0].contains("Q4 numbers.xlsx"),
        "the heading was stepped over: {marked:?}"
    );
}

/// AC-3: `s` saves the highlighted part alone and names the file that was written.
#[tokio::test]
async fn s_saves_the_highlighted_part_and_names_the_written_file() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    let dir = saved_dir(&f);
    answer_save(&f, vec![saved_result("0.1", &dir.join("plan.pdf"), 2048)]);
    let effects = f.screen.update(Msg::Key(key(KeyCode::Char('s'))));
    assert_eq!(f.screen.status.notice(), Some("Saving…"));
    settle_capturing(&mut f.screen, effects, &f.last_theme, &f.editor).await;
    assert_eq!(
        calls(&f),
        [save_call("work", "m1", &["0.1"], &saved_text(&f))]
    );
    assert!(f.screen.modal.is_none(), "the list closed");
    assert_eq!(
        f.screen.status.notice(),
        Some(format!("Saved plan.pdf to {}", saved_text(&f)).as_str())
    );
}

/// AC-4: `S` saves every attachment of the highlighted row's message.
#[tokio::test]
async fn shift_s_saves_every_attachment_of_that_message() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "logo.png").await;
    let dir = saved_dir(&f);
    answer_save(
        &f,
        vec![
            saved_result("0.1", &dir.join("plan.pdf"), 2048),
            saved_result("0.2", &dir.join("logo.png"), 512),
        ],
    );
    press(&mut f, KeyCode::Char('S')).await;
    assert_eq!(calls(&f), [save_call("work", "m1", &[], &saved_text(&f))]);
    assert_eq!(
        f.screen.status.notice(),
        Some(format!("Saved 2 attachments to {}", saved_text(&f)).as_str())
    );
}

/// AC-5: `w` saves the cursor message's attachments without opening the list.
#[tokio::test]
async fn w_saves_the_cursor_message_s_attachments_without_the_modal() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('w')).await;
    assert_eq!(calls(&f), [save_call("work", "m2", &[], &saved_text(&f))]);
    assert!(f.screen.modal.is_none(), "no list is drawn");
    let (mut f, _) = attachment_fixture().await;
    cursor_onto_message(&mut f, "m1").await;
    press(&mut f, KeyCode::Char('w')).await;
    assert_eq!(calls(&f), [save_call("work", "m1", &[], &saved_text(&f))]);
}

/// AC-6: `w` on a message with no attachments says so and sends nothing.
#[tokio::test]
async fn w_on_a_message_without_attachments_says_so() {
    let (mut f, _) = attachment_fixture_with(PARTS, &["m3"]).await;
    press(&mut f, KeyCode::Char('w')).await;
    assert!(calls(&f).is_empty(), "{:?}", calls(&f));
    assert_eq!(
        f.screen.status.notice(),
        Some("This message has no attachments.")
    );
}

/// AC-6: `w` on a line that belongs to no message asks for one, and sends nothing.
#[tokio::test]
async fn w_on_a_line_of_no_message_says_so() {
    let (mut f, _) = attachment_fixture().await;
    cursor_onto_no_message(&mut f).await;
    press(&mut f, KeyCode::Char('w')).await;
    assert!(calls(&f).is_empty(), "{:?}", calls(&f));
    assert_eq!(f.screen.status.notice(), Some("Move onto a message first."));
}

/// AC-7: `Enter` asks before the session's first open, then hands the answered path to the
/// system opener.
#[tokio::test]
async fn enter_asks_once_then_opens_with_the_system_opener() {
    let (mut f, opened) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(calls(&f).is_empty(), "nothing is sent yet: {:?}", calls(&f));
    let text = draw(&f, 90, 24);
    assert!(text.contains("┌ Open this attachment? "), "{text}");
    assert!(text.contains("y yes · n or Esc no"), "{text}");
    let Some(Modal::Confirm(confirm)) = &f.screen.modal else {
        panic!("no question");
    };
    assert_eq!(confirm.lines(), OPEN_LINES);
    let path = f.paths.attachments_dir().join("tok1").join("plan.pdf");
    answer_open(
        &f,
        Ok(AttachmentOpenResponse {
            path: path.display().to_string(),
        }),
    );
    let effects = f.screen.update(Msg::Key(key(KeyCode::Char('y'))));
    assert_eq!(f.screen.status.notice(), Some("Opening…"));
    settle_capturing(&mut f.screen, effects, &f.last_theme, &f.editor).await;
    assert_eq!(
        calls(&f),
        [FakeCall::AttachmentOpen {
            account: "work".into(),
            message_id: "m1".into(),
            part_id: "0.1".into(),
        }]
    );
    assert_eq!(
        *opened.lock().unwrap(),
        [Url::from_file_path(&path).unwrap()]
    );
}

/// AC-8: the open question is asked once a session.
#[tokio::test]
async fn the_open_question_is_asked_once_a_session() {
    let (mut f, _) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    assert!(f.screen.modal.is_none(), "no question the second time");
    assert_eq!(
        calls(&f),
        [FakeCall::AttachmentOpen {
            account: "work".into(),
            message_id: "m1".into(),
            part_id: "0.1".into(),
        }]
    );
}

/// AC-9: `n` cancels the open and the list does not come back.
#[tokio::test]
async fn n_cancels_the_open_and_the_list_is_gone() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('n')).await;
    assert!(calls(&f).is_empty(), "{:?}", calls(&f));
    assert!(f.screen.modal.is_none(), "no modal is drawn");
}

/// AC-10: `Enter` on a type Rail does not open saves that part instead, and says so.
#[tokio::test]
async fn enter_on_a_type_rail_does_not_open_saves_it_instead() {
    let mut parts = PARTS.to_vec();
    parts.push(("m2", "0.3", "diagram.svg", 1024, "image/svg+xml"));
    let (mut f, opened) = attachment_fixture_with(&parts, &[]).await;
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "diagram.svg").await;
    let dir = saved_dir(&f);
    answer_save(
        &f,
        vec![saved_result("0.3", &dir.join("diagram.svg"), 1024)],
    );
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        calls(&f),
        [save_call("work", "m2", &["0.3"], &saved_text(&f))]
    );
    assert!(opened.lock().unwrap().is_empty(), "nothing was opened");
    assert_eq!(
        f.screen.status.notice(),
        Some(
            format!(
                "Rail does not open image/svg+xml files; saved diagram.svg to {}",
                saved_text(&f)
            )
            .as_str()
        )
    );
}

/// AC-11: `Esc`, `q` and `a` each close the list with nothing sent.
#[tokio::test]
async fn esc_q_and_a_close_the_list_without_sending_anything() {
    for code in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('a')] {
        let (mut f, _) = attachment_fixture().await;
        press(&mut f, KeyCode::Char('a')).await;
        press(&mut f, code).await;
        assert!(f.screen.modal.is_none(), "{code:?} left a modal");
        assert!(calls(&f).is_empty(), "{code:?} sent {:?}", calls(&f));
    }
}

/// AC-12: a list taller than the box scrolls and keeps the cursor drawn, in both icon sets.
#[tokio::test]
async fn a_long_list_scrolls_and_keeps_the_cursor_drawn() {
    let names: Vec<(String, String)> = (1..=22)
        .map(|n| (format!("0.{n}"), format!("attachment-{n}.pdf")))
        .collect();
    let parts: Vec<(&str, &str, &str, i64, &str)> = names
        .iter()
        .map(|(part, name)| ("m1", part.as_str(), name.as_str(), 2048, "application/pdf"))
        .collect();
    for (icons, name, up, down) in [
        (Icons::Unicode, "unicode", "↑ more", "↓ more"),
        (Icons::Ascii, "ascii", "^ more", "v more"),
    ] {
        let (mut f, _) = attachment_fixture_icons(&parts, &[], icons).await;
        press(&mut f, KeyCode::Char('a')).await;
        for _ in 0..21 {
            press(&mut f, KeyCode::Char('j')).await;
        }
        let text = draw(&f, 90, 24);
        assert!(text.contains(up), "{text}");
        assert!(!text.contains(down), "the last row is drawn: {text}");
        let marked: Vec<String> = rows(&f, 90, 24)
            .into_iter()
            .filter(|row| row.contains("attachment-22.pdf"))
            .collect();
        assert_eq!(marked.len(), 1, "{marked:?}");
        let cursor = match icons {
            Icons::Unicode => "▸ ",
            Icons::Ascii => "> ",
        };
        assert!(marked[0].contains(cursor), "{marked:?}");
        insta::assert_snapshot!(
            format!("attachments_modal_scrolled_{name}_90x24"),
            draw(&f, 90, 24)
        );
    }
}

/// AC-13: the modal drawn at both sizes in both icon sets; inside the box only the marker
/// column changes between them.
#[tokio::test]
async fn attachments_modal_snapshots() {
    let mut frames: Vec<(&str, (u16, u16), Vec<String>)> = Vec::new();
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        let (mut f, _) = attachment_fixture_icons(PARTS, &[], icons).await;
        press(&mut f, KeyCode::Char('a')).await;
        for (width, height) in [(90, 24), (150, 40)] {
            insta::assert_snapshot!(
                format!("attachments_modal_{name}_{width}x{height}"),
                draw(&f, width, height)
            );
            frames.push((name, (width, height), rows(&f, width, height)));
        }
    }
    for (width, height) in [(90, 24), (150, 40)] {
        let frame = |wanted: &str| {
            frames
                .iter()
                .find(|(name, size, _)| *name == wanted && *size == (width, height))
                .map(|(_, _, rows)| rows.clone())
                .expect("the frame was drawn")
        };
        let unicode = frame("unicode");
        let ascii = frame("ascii");
        let top = unicode
            .iter()
            .position(|row| row.contains("┌ Attachments "))
            .expect("the box's top border");
        let bottom = unicode
            .iter()
            .skip(top)
            .position(|row| row.contains('└'))
            .map(|at| at + top)
            .expect("the box's bottom border");
        assert!(bottom > top + 1, "the box has rows inside it");
        // Only the box is compared: the mail screen behind it swaps many glyphs between the
        // icon sets, while `boxed` draws Unicode borders in both (§6 slot 10).
        let left = box_left(&unicode[top]);
        let cells: Vec<char> = unicode[top].chars().collect();
        let right = cells[left..]
            .iter()
            .position(|c| *c == '┐')
            .map(|at| at + left)
            .expect("the box's right edge");
        let inside =
            |row: &str| -> String { row.chars().skip(left).take(right - left + 1).collect() };
        for y in (top + 1)..bottom {
            assert_eq!(
                inside(&ascii[y]),
                inside(&unicode[y]).replace('▸', ">"),
                "row {y} differs outside the marker column"
            );
        }
        let box_rows: Vec<String> = ascii[top + 1..bottom].iter().map(|r| inside(r)).collect();
        assert!(
            box_rows.iter().any(|row| row.contains("> plan.pdf")),
            "{box_rows:?}"
        );
        assert!(
            box_rows.iter().all(|row| !row.contains('▸')),
            "no Unicode marker in the ASCII box: {box_rows:?}"
        );
    }
}

/// AC-14: the modal drawn into areas too small for it, without a panic or a loop.
#[tokio::test]
async fn attachments_modal_survives_tiny_areas() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    let modal = f.screen.modal.as_ref().expect("the list is open");
    for (width, height) in [(0u16, 24u16), (1, 24), (90, 3), (3, 2), (6, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).expect("test terminal");
        terminal
            .draw(|frame| {
                modal.render(frame, Rect::new(0, 0, width, height), &Theme::default());
            })
            .expect("draw");
    }
}

/// AC-28: the copies `attachment.open` wrote go when the reader closes, and when the screen is
/// dropped with the reader still open.
#[tokio::test]
async fn closing_the_reader_removes_what_it_opened() {
    // Two attachments, as AC-28 asks: the first answers the session's open question, the second
    // goes straight through. Each gets its own token directory, so a `remove_all` that removed
    // only the last path would fail here.
    let (mut f, opened) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('y')).await;
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "logo.png").await;
    press(&mut f, KeyCode::Char('o')).await;
    let written: Vec<PathBuf> = opened
        .lock()
        .unwrap()
        .iter()
        .map(|url| url.to_file_path().expect("a file URL"))
        .collect();
    assert_eq!(written.len(), 2, "{written:?}");
    for path in &written {
        assert!(path.is_file(), "the fake wrote {written:?}");
    }
    let dirs: Vec<&std::path::Path> = written
        .iter()
        .map(|path| path.parent().expect("a token directory"))
        .collect();
    assert_ne!(dirs[0], dirs[1], "a token directory each: {written:?}");
    press(&mut f, KeyCode::Char('q')).await;
    for path in &written {
        assert!(!path.exists(), "the file stayed: {path:?}");
    }
    for dir in &dirs {
        assert!(!dir.exists(), "the token directory stayed: {dir:?}");
    }

    let (mut f, opened) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    let path = opened.lock().unwrap()[0]
        .to_file_path()
        .expect("a file URL");
    assert!(path.is_file());
    let Fixture { _tmp, screen, .. } = f;
    drop(screen);
    assert!(!path.exists(), "dropping the screen removed it");
}

/// The modal's row and the open target, each printed through `Debug`.
fn printed_rows() -> [String; 2] {
    let row = AttachmentRow {
        account: "work".into(),
        message: "m1".into(),
        part_id: "0.1".into(),
        name: "plan.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 2048,
        message_index: 0,
    };
    let target = AttachmentTarget {
        account: "work".into(),
        message: "m1".into(),
        part_id: "0.1".into(),
        filename: "plan.pdf".into(),
        mime_type: "application/pdf".into(),
    };
    [format!("{row:?}"), format!("{target:?}")]
}

/// The save's answer and the open's, each printed through `Debug`.
fn printed_events() -> [String; 2] {
    let saved = MailEvent::AttachmentSaved {
        result: Ok(AttachmentSaveResponse {
            results: vec![AttachmentResult {
                part_id: "0.1".into(),
                outcome: AttachmentOutcome::Saved {
                    path: "/x/plan.pdf".into(),
                    size: 1,
                },
            }],
        }),
        account: "work".into(),
        dir: "/x".into(),
        single: true,
        name: Some("plan.pdf".into()),
        unopenable: Some(crate::tui::data::NotOpened::Type("application/pdf".into())),
    };
    let openend = MailEvent::AttachmentOpened {
        result: Ok(AttachmentOpenResponse {
            path: "/x/plan.pdf".into(),
        }),
        account: "work".into(),
        message: "m1".into(),
        part_id: "0.1".into(),
        filename: "plan.pdf".into(),
    };
    [format!("{saved:?}"), format!("{openend:?}")]
}

/// AC-32 (the `rail` half): neither the modal's rows nor the two events print a name, a type, a
/// directory, a path or an account.
#[tokio::test]
async fn debug_of_the_rows_and_the_events_prints_counts_only() {
    let printed = [printed_rows(), printed_events()].concat();
    for text in &printed {
        for secret in ["plan", "/x", "work", "application/pdf", "0.1", "m1"] {
            assert!(!text.contains(secret), "{text} carries {secret}");
        }
    }
    assert!(printed[2].contains("ok"), "{}", printed[2]);
    assert!(printed[2].contains("single"), "{}", printed[2]);
    assert!(printed[3].contains("ok"), "{}", printed[3]);
}
