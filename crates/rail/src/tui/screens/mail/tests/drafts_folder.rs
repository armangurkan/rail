//! An account's Drafts folder: local, pushed and Gmail drafts in one pane, Enter resuming or
//! importing, `d` discarding each kind, thread actions refused, the pane read again on a
//! change, and `:drafts` reading the same rows for every account in the unified inbox.

use rail_core::config::Icons;
use rail_core::ops::{DraftImportResponse, ErrorCode};
use rail_core::provider::{Envelope, FolderKind, Mailbox, RemoteFolder};
use rail_core::store::drafts::{DraftKind, DraftWrite, Recipient};

use super::compose::{calls_of, compose_open, composing};
use super::*;
use crate::tui::modals::Target;

/// When the seeded drafts were saved: after every fixture message, a minute apart.
const LOCAL_AT: i64 = BASE_MS / 1000 + 3_600;

fn recipient(name: Option<&str>, address: &str) -> Recipient {
    Recipient {
        name: name.map(str::to_string),
        address: address.into(),
    }
}

/// A local draft of account `account_id` saved at `at`; it must get `id`.
fn seed_local(f: &Fixture, id: i64, account_id: i64, subject: &str, to: Recipient, at: i64) {
    let write = DraftWrite {
        id: None,
        account_id,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![to],
        cc: vec![],
        bcc: vec![],
        subject: subject.into(),
        body: format!("body of {subject}\n"),
        attachments: vec![],
        message_id: format!("<d{id}@example.com>"),
    };
    let made = writer(f).draft_upsert(&write, at).unwrap();
    assert_eq!(made, Some(id), "drafts are numbered in order");
}

/// A synced Gmail draft of `work` in the Drafts folder only.
fn gmail_draft(remote: &str, subject: &str, to: &str, secs: i64) -> Envelope {
    Envelope {
        is_draft: true,
        subject: subject.into(),
        to: vec![Mailbox {
            name: Some("Gail".into()),
            address: to.into(),
        }],
        folders: vec!["DRAFT".into()],
        ..mail_envelope(remote, &format!("t-{remote}"), secs * 1000)
    }
}

/// `work`'s folders with Gmail's Drafts added, and its Gmail drafts.
fn sync_drafts(f: &Fixture, upserts: Vec<Envelope>) {
    let folders = vec![
        ("INBOX", "Inbox", FolderKind::Inbox),
        ("Label_1", "Projects", FolderKind::User),
        ("SENT", "Sent", FolderKind::Sent),
        ("DRAFT", "Drafts", FolderKind::Drafts),
    ]
    .into_iter()
    .map(|(remote_id, name, kind)| RemoteFolder {
        remote_id: remote_id.into(),
        name: name.into(),
        kind,
    })
    .collect();
    let batch = Batch {
        folders: Some(folders),
        upserts,
        ..Batch::new(1)
    };
    writer(f).apply(&batch, BASE_MS / 1000).unwrap();
}

fn message_id(f: &Fixture, remote: &str) -> i64 {
    writer(f)
        .conn()
        .query_row(
            "SELECT id FROM messages WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

/// `fixture(3)` whose `work` has a Drafts folder holding, newest first: draft 2 (pushed, its
/// Gmail copy `dm1`), the Gmail draft `dm2` no local draft owns, and draft 1 (local only).
async fn drafts_fixture() -> Fixture {
    let mut f = fixture(3).await;
    seed_local(
        &f,
        1,
        1,
        "Local plan",
        recipient(None, "lee@example.com"),
        LOCAL_AT,
    );
    seed_local(
        &f,
        2,
        1,
        "Pushed note",
        recipient(Some("Pat Doe"), "pat@example.com"),
        LOCAL_AT + 120,
    );
    writer(&f).draft_set_remote(2, "r1", "dm1").unwrap();
    sync_drafts(
        &f,
        vec![
            gmail_draft("dm1", "Pushed note", "pat@example.com", LOCAL_AT + 120),
            gmail_draft(
                "dm2",
                "Written on the phone",
                "gail@example.com",
                LOCAL_AT + 60,
            ),
        ],
    );
    store_changed(&mut f).await;
    f
}

/// The sidebar's index of `work`'s Drafts folder.
fn drafts_item(f: &Fixture) -> usize {
    f.screen
        .sidebar
        .items()
        .iter()
        .position(|i| i.label == "Drafts" && i.account == Some(0))
        .expect("work's Drafts folder is in the sidebar")
}

/// Focus the sidebar and walk its cursor to the Drafts folder; Enter is left to the caller.
async fn point_at_drafts(f: &mut Fixture) {
    press(f, KeyCode::Tab).await;
    let target = drafts_item(f);
    while f.screen.sidebar.cursor() < target {
        press(f, KeyCode::Char('j')).await;
    }
}

async fn open_drafts(f: &mut Fixture) {
    point_at_drafts(f).await;
    press(f, KeyCode::Enter).await;
    assert!(f.screen.in_drafts());
}

fn pane_subjects(f: &Fixture) -> Vec<String> {
    f.screen
        .drafts
        .pane
        .rows()
        .iter()
        .map(|r| r.subject.clone())
        .collect()
}

fn imports(f: &Fixture) -> Vec<(String, i64)> {
    calls_of(f, |c| matches!(c, FakeCall::DraftImport { .. }))
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::DraftImport {
                account, message, ..
            } => Some((account, message)),
            _ => None,
        })
        .collect()
}

fn discards(f: &Fixture) -> Vec<i64> {
    calls_of(f, |c| matches!(c, FakeCall::DraftDiscard { .. }))
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::DraftDiscard { draft } => Some(draft),
            _ => None,
        })
        .collect()
}

fn drawn(f: &Fixture, width: u16, height: u16) -> String {
    crate::tui::test_util::render_lines(width, height, |frame| {
        f.screen.view(frame, &Theme::default());
    })
    .join("\n")
}

/// Run `effects` one job at a time, returning every event they report, without feeding them.
async fn events_of(effects: Vec<Effect>) -> Vec<TaskResult> {
    let mut all = Vec::new();
    for effect in effects {
        all.extend(run_job(effect).await);
    }
    all
}

#[tokio::test]
async fn the_drafts_folder_lists_local_pushed_and_gmail_rows() {
    let mut f = drafts_fixture().await;
    point_at_drafts(&mut f).await;
    let effects = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let reported = events_of(effects).await;
    assert!(
        reported
            .iter()
            .any(|r| matches!(r, TaskResult::Mail(MailEvent::DraftsFolder { .. }))),
        "a drafts_folder read goes out: {reported:?}"
    );
    assert!(
        !reported
            .iter()
            .any(|r| matches!(r, TaskResult::Mail(MailEvent::Listed { .. }))),
        "no thread list read: {reported:?}"
    );
    let more = feed(&mut f, reported);
    settle(&mut f.screen, more).await;
    assert!(f.screen.in_drafts());
    assert_eq!(f.screen.focus, Focus::List);
    assert_eq!(
        pane_subjects(&f),
        ["Pushed note", "Written on the phone", "Local plan"]
    );
    let shown = drawn(&f, 150, 30);
    let line = |subject: &str| {
        shown
            .lines()
            .find(|l| l.contains(subject))
            .unwrap_or_else(|| panic!("{subject} is drawn:\n{shown}"))
            .to_string()
    };
    let local = line("Local plan");
    assert!(local.contains("(local)"), "{local}");
    assert!(local.contains("✎ Local plan"), "{local}");
    let pushed = line("Pushed note");
    assert!(pushed.contains("Pat Doe"), "{pushed}");
    assert!(!pushed.contains("(local)"), "{pushed}");
    let gmail = line("Written on the phone");
    assert!(gmail.contains("Gail"), "{gmail}");
    assert!(!gmail.contains("(local)"), "{gmail}");
}

#[tokio::test]
async fn enter_resumes_a_local_draft_and_imports_a_gmail_one() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(compose_open(&f), "the review opens on the local draft");
    assert_eq!(composing(&f).draft, Some(1));
    assert!(imports(&f).is_empty(), "a local draft is not imported");
    press(&mut f, KeyCode::Char('q')).await;
    assert!(!compose_open(&f));
    assert!(f.screen.in_drafts(), "back in the folder");
    // The daemon makes the Gmail draft local draft 3 and answers it.
    seed_local(
        &f,
        3,
        1,
        "Written on the phone",
        recipient(Some("Gail"), "gail@example.com"),
        LOCAL_AT + 60,
    );
    writer(&f).draft_set_remote(3, "r2", "dm2").unwrap();
    *f.fake.import_answer.lock().unwrap() = Some(Ok(DraftImportResponse { draft: 3 }));
    press(&mut f, KeyCode::Char('k')).await;
    let effects = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let line = f.screen.status_line();
    assert!(line.contains("fetching the draft…"), "{line}");
    settle(&mut f.screen, effects).await;
    assert_eq!(imports(&f), [("work".to_string(), message_id(&f, "dm2"))]);
    assert!(compose_open(&f), "the review opens on the imported draft");
    assert_eq!(composing(&f).draft, Some(3));
    assert!(composing(&f).body.contains("Written on the phone"));
}

#[tokio::test]
async fn trash_asks_then_discards_each_kind() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    press(&mut f, KeyCode::Char('d')).await;
    assert!(matches!(f.screen.modal, Some(Modal::Confirm(_))));
    let shown = drawn(&f, 90, 24);
    assert!(shown.contains("Discard this draft? (y/n)"), "{shown}");
    assert!(discards(&f).is_empty(), "asks first");
    // The daemon deletes it, as `draft.discard` would.
    writer(&f).draft_delete(1).unwrap();
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(discards(&f), [1]);
    assert!(f.screen.status_line().contains("draft discarded"));
    assert_eq!(
        pane_subjects(&f),
        ["Pushed note", "Written on the phone"],
        "the pane is read again"
    );
    // The Gmail row: imported first, then the answered draft discarded.
    *f.fake.import_answer.lock().unwrap() = Some(Ok(DraftImportResponse { draft: 3 }));
    press(&mut f, KeyCode::Home).await;
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Char('d')).await;
    let shown = drawn(&f, 90, 24);
    assert!(shown.contains("Discard this draft? (y/n)"), "{shown}");
    assert!(
        shown.contains("Gmail"),
        "says Gmail's copy goes too:\n{shown}"
    );
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(imports(&f), [("work".to_string(), message_id(&f, "dm2"))]);
    assert_eq!(discards(&f), [1, 3], "the imported draft is discarded");
    assert!(!compose_open(&f), "nothing opens");
    // The pushed row while its push is in flight: the refusal is on the bar.
    *f.fake.discard_answer.lock().unwrap() = Some(Err(OpError::invalid_input(
        "draft: saving to Gmail, try again in a moment",
    )));
    press(&mut f, KeyCode::Home).await;
    press(&mut f, KeyCode::Char('d')).await;
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(discards(&f), [1, 3, 2]);
    let line = f.screen.status_line();
    assert!(
        line.contains("draft: saving to Gmail, try again in a moment"),
        "{line}"
    );
    // `n` keeps it.
    press(&mut f, KeyCode::Char('d')).await;
    press(&mut f, KeyCode::Char('n')).await;
    assert_eq!(discards(&f), [1, 3, 2]);
}

#[tokio::test]
async fn thread_actions_are_not_available_on_drafts() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    let before = f.fake.calls.lock().unwrap().len();
    for c in ['e', 's', 'u', 'l', 'm', ' ', 'r', 'R', 'f', '/'] {
        f.screen.status.notify("", f.screen.tick);
        press(&mut f, KeyCode::Char(c)).await;
        let line = f.screen.status_line();
        assert!(line.contains("Not available on drafts"), "{c:?}: {line}");
        assert!(f.screen.modal.is_none(), "{c:?} opens nothing");
        assert!(!compose_open(&f), "{c:?} opens nothing");
    }
    assert!(f.screen.actions.selection.is_empty());
    assert_eq!(f.fake.calls.lock().unwrap().len(), before, "no call");
    assert_eq!(pane_subjects(&f).len(), 3);
}

#[tokio::test]
async fn a_failed_import_says_so_and_stays_in_the_folder() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    *f.fake.import_answer.lock().unwrap() = Some(Err(OpError::new(
        ErrorCode::NotFound,
        rail_core::error::ErrorClass::Permanent,
        "Gmail no longer has this draft",
    )));
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(imports(&f).len(), 1);
    let line = f.screen.status_line();
    assert!(line.contains("Gmail no longer has this draft"), "{line}");
    assert!(!compose_open(&f));
    assert!(f.screen.in_drafts());
    assert_eq!(pane_subjects(&f).len(), 3, "the pane stays");
    assert_eq!(f.screen.drafts.pane.cursor(), 1);
    // Another Enter tries again: nothing is left pending.
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(imports(&f).len(), 2);
}

#[tokio::test]
async fn a_pane_enter_while_an_import_is_pending_says_busy() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    press(&mut f, KeyCode::Char('j')).await;
    let pending = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    assert_eq!(pending.len(), 1, "the import call");
    for code in [KeyCode::Enter, KeyCode::Char('d')] {
        let effects = f.screen.update(Msg::Key(key(code)));
        if code == KeyCode::Char('d') {
            let more = f.screen.update(Msg::Key(key(KeyCode::Char('y'))));
            assert!(more.is_empty(), "no discard while importing");
        }
        assert!(effects.is_empty(), "{code:?} starts nothing");
        let line = f.screen.status_line();
        assert!(line.contains(composing::BUSY), "{line}");
    }
    *f.fake.import_answer.lock().unwrap() = Some(Ok(DraftImportResponse { draft: 2 }));
    settle(&mut f.screen, pending).await;
    assert!(compose_open(&f), "the import's answer still opens it");
    assert_eq!(composing(&f).draft, Some(2));
}

#[tokio::test]
async fn colon_drafts_lists_every_account_in_the_unified_inbox() {
    let mut f = drafts_fixture().await;
    seed_local(
        &f,
        3,
        2,
        "Home errand",
        recipient(None, "kim@example.com"),
        LOCAL_AT + 600,
    );
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
    let effects = f.screen.run_palette(Target::Drafts);
    let reported = events_of(effects).await;
    assert!(
        reported
            .iter()
            .any(|r| matches!(r, TaskResult::Mail(MailEvent::DraftsFolder { .. }))),
        "{reported:?}"
    );
    let more = feed(&mut f, reported);
    settle(&mut f.screen, more).await;
    assert!(matches!(f.screen.modal, Some(Modal::Picker(_))));
    let shown = drawn(&f, 150, 30);
    for subject in [
        "Home errand",
        "Pushed note",
        "Written on the phone",
        "Local plan",
    ] {
        assert!(shown.contains(subject), "{subject}:\n{shown}");
    }
    // The second row is the pushed draft; the third, Gmail's, imports.
    press(&mut f, KeyCode::Down).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(compose_open(&f));
    assert_eq!(composing(&f).draft, Some(2));
    press(&mut f, KeyCode::Char('q')).await;
    let effects = f.screen.run_palette(Target::Drafts);
    settle(&mut f.screen, effects).await;
    *f.fake.import_answer.lock().unwrap() = Some(Ok(DraftImportResponse { draft: 1 }));
    press_n(&mut f, KeyCode::Down, 2).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(imports(&f), [("work".to_string(), message_id(&f, "dm2"))]);
    assert_eq!(composing(&f).draft, Some(1));
    // In an account's scope, only that account's drafts.
    press(&mut f, KeyCode::Char('q')).await;
    press(&mut f, KeyCode::Char('2')).await;
    let effects = f.screen.run_palette(Target::Drafts);
    settle(&mut f.screen, effects).await;
    let shown = drawn(&f, 150, 30);
    assert!(shown.contains("Home errand"), "{shown}");
    assert!(!shown.contains("Local plan"), "{shown}");
}

#[tokio::test]
async fn the_pane_is_read_again_when_the_outbox_or_sync_changes() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    press(&mut f, KeyCode::Char('j')).await;
    seed_local(
        &f,
        3,
        1,
        "Newest",
        recipient(None, "new@example.com"),
        LOCAL_AT + 900,
    );
    store_changed(&mut f).await;
    assert_eq!(
        pane_subjects(&f),
        [
            "Newest",
            "Pushed note",
            "Written on the phone",
            "Local plan"
        ]
    );
    assert_eq!(
        f.screen.drafts.pane.selected().map(|r| r.subject.as_str()),
        Some("Written on the phone"),
        "the cursor stays on its row, one place down"
    );
    // The outbox: a push lands and the Gmail draft becomes the local draft's copy.
    writer(&f).draft_set_remote(3, "r3", "dm2").unwrap();
    let mut saving = status("work", "w@example.com", SyncPhase::Idle);
    saving.outbox.saving = 1;
    let event = MailEvent::Daemon(DaemonEvent::Progress(Box::new(saving)));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    assert_eq!(pane_subjects(&f), ["Newest", "Pushed note", "Local plan"]);
    // Leaving the folder reads the thread list again.
    press(&mut f, KeyCode::Char('0')).await;
    assert!(!f.screen.in_drafts());
    assert_eq!(f.screen.list.rows().len(), 5);
}

#[tokio::test]
async fn snapshot_drafts_folder_90_and_150_columns() {
    for (width, height, icons) in [(90, 24, Icons::Unicode), (150, 40, Icons::Ascii)] {
        let mut f = drafts_fixture().await;
        f.screen.ctx.icons = icons;
        open_drafts(&mut f).await;
        let shot = crate::tui::test_util::snapshot(width, height, |frame| {
            f.screen.view(frame, &Theme::default())
        });
        insta::assert_snapshot!(format!("drafts_folder_{width}"), shot);
    }
}
