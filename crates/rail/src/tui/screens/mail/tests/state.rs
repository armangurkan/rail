//! The mail screen's state across store changes: renamed and deleted labels, failed reads in
//! their own areas, the daemon coming back through a fetch, HTML answers for another thread,
//! focus on re-reads, and refresh gating when an unrelated job panics.

use crossterm::event::KeyCode;
use rail_core::error::ErrorClass;
use rail_core::ops::{ErrorCode, OpError, SyncPhase};
use rail_core::provider::{FolderKind, RemoteFolder};
use rail_core::store::view::HtmlBody;
use rail_core::store::view::MAX_SIDEBAR_FOLDERS;
use rail_core::sync::Batch;

use super::{Fixture, feed, fixture, press, press_n, run_job, selected, store_changed, writer};
use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{DaemonEvent, FakeCall, MailEvent, ReadError, ScopeChoice};
use crate::tui::screen::Screen;
use crate::tui::screens::mail::{Focus, StoreState};
use crate::tui::test_util::{
    BASE_MS, ctrl, mail_envelope, recording_opener, render_lines, settle, status,
};
use crate::tui::theme::Theme;

/// The screen at 150 columns: sidebar, list and reader pane all shown.
fn draw(f: &Fixture) -> Vec<String> {
    render_lines(150, 40, |frame| f.screen.view(frame, &Theme::default()))
}

/// Account 1's folder list as a labels step sends it: Inbox, Sent and these user labels
/// (remote id, name).
fn labels(user: &[(&str, &str)]) -> Batch {
    let mut folders = vec![
        RemoteFolder {
            remote_id: "INBOX".into(),
            name: "Inbox".into(),
            kind: FolderKind::Inbox,
        },
        RemoteFolder {
            remote_id: "SENT".into(),
            name: "Sent".into(),
            kind: FolderKind::Sent,
        },
    ];
    folders.extend(user.iter().map(|(id, name)| RemoteFolder {
        remote_id: (*id).into(),
        name: (*name).into(),
        kind: FolderKind::User,
    }));
    Batch {
        folders: Some(folders),
        ..Batch::new(1)
    }
}

/// Review Focus 2.
#[tokio::test]
async fn a_selected_label_renamed_then_deleted_keeps_then_moves_the_cursor_safely() {
    let mut f = fixture(3).await;
    let mut store = writer(&f);
    let mut labelled = mail_envelope("wl", "twl", BASE_MS - 60_000);
    labelled.folders = vec!["Label_1".into()];
    store
        .apply(
            &Batch {
                upserts: vec![labelled],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    store_changed(&mut f).await;
    // Sidebar: All inboxes, work, Sent, Projects, home, Sent, Projects.
    press(&mut f, KeyCode::Tab).await;
    press_n(&mut f, KeyCode::Char('j'), 3).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(selected(&f).as_deref(), Some("subject wl"));

    // Renamed in Gmail: the same label id under a new name.
    store
        .apply(&labels(&[("Label_1", "Clients")]), BASE_MS / 1000)
        .unwrap();
    store_changed(&mut f).await;
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Folder { account: 0, name, .. } if name == "Clients"),
        "{:?}",
        f.screen.scope
    );
    assert_eq!(
        f.screen.sidebar.cursor(),
        3,
        "the cursor stayed on the label"
    );
    assert_eq!(
        f.screen.sidebar.selected().map(|i| i.label.as_str()),
        Some("Clients")
    );
    let header = draw(&f).remove(0);
    assert!(header.contains("work · Clients · 1 unread"), "{header}");
    assert_eq!(selected(&f).as_deref(), Some("subject wl"));

    // Then deleted: the account's inbox, its sidebar row, the list read again.
    store.apply(&labels(&[]), BASE_MS / 1000).unwrap();
    store_changed(&mut f).await;
    assert_eq!(f.screen.scope, ScopeChoice::Account(0));
    assert_eq!(f.screen.sidebar.cursor(), 1);
    assert_eq!(
        f.screen.sidebar.selected().map(|i| i.label.as_str()),
        Some("work")
    );
    assert!(
        f.screen
            .sidebar
            .items()
            .iter()
            .all(|i| i.label != "Clients")
    );
    assert_eq!(f.screen.list.rows().len(), 3, "work's inbox");
    assert_eq!(selected(&f).as_deref(), Some("subject wm2"));
    assert!(
        f.screen
            .status
            .notice()
            .is_some_and(|n| n.contains("The label you had open is gone")),
        "{:?}",
        f.screen.status.notice()
    );
}

#[tokio::test]
async fn a_label_pushed_past_the_sidebar_cap_is_not_called_deleted() {
    let mut f = fixture(3).await;
    let mut store = writer(&f);
    // Sidebar: All inboxes, work, Sent, Projects, home, Sent, Projects.
    press(&mut f, KeyCode::Tab).await;
    press_n(&mut f, KeyCode::Char('j'), 3).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(matches!(
        f.screen.scope,
        ScopeChoice::Folder { account: 0, .. }
    ));

    // The label is renamed to sort last among more labels than the sidebar lists.
    let others: Vec<(String, String)> = (0..MAX_SIDEBAR_FOLDERS + 20)
        .map(|n| (format!("Other_{n}"), format!("L{n:04}")))
        .collect();
    let mut user: Vec<(&str, &str)> = others
        .iter()
        .map(|(id, name)| (id.as_str(), name.as_str()))
        .collect();
    user.push(("Label_1", "zzz"));
    store.apply(&labels(&user), BASE_MS / 1000).unwrap();
    store_changed(&mut f).await;
    assert_eq!(f.screen.scope, ScopeChoice::Account(0));
    let notice = f.screen.status.notice().unwrap_or_default().to_string();
    assert!(notice.contains("more than 500 labels"), "{notice}");
    assert!(!notice.contains("is gone"), "{notice}");
}

#[tokio::test]
async fn a_failed_sidebar_read_stays_in_the_sidebar_and_the_list_keeps_its_rows() {
    let mut f = fixture(3).await;
    let failed = MailEvent::Sidebar {
        seq: f.screen.latest_sidebar,
        result: Err(ReadError::Failed(OpError::internal("disk I/O error"))),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(failed)));
    settle(&mut f.screen, effects).await;
    assert_eq!(f.screen.store_state, StoreState::Ready, "the list stays");
    assert_eq!(f.screen.list.rows().len(), 5);
    assert!(f.screen.sidebar.failed());
    let line = f.screen.status_line();
    assert!(
        line.contains("The sidebar could not be read: disk I/O error"),
        "{line}"
    );
    assert_eq!(f.screen.error_count(), 1, ":errors keeps it");
    let screen = draw(&f).join("\n");
    assert!(screen.contains("Could not read folders"), "{screen}");
    assert!(screen.contains("subject wm2"), "{screen}");
    store_changed(&mut f).await;
    assert!(!f.screen.sidebar.failed(), "the next good read clears it");
}

#[tokio::test]
async fn a_failed_reread_of_the_open_thread_shows_in_the_reader_not_the_list() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Enter).await;
    let thread_id = f.screen.reader.as_ref().unwrap().thread_id();
    let failed = MailEvent::Thread {
        seq: f.screen.latest_thread,
        thread_id,
        result: Err(ReadError::Failed(OpError::internal("disk I/O error"))),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(failed)));
    settle(&mut f.screen, effects).await;
    assert_eq!(f.screen.store_state, StoreState::Ready, "the list stays");
    assert_eq!(f.screen.list.rows().len(), 5);
    assert!(f.screen.reader.is_some(), "the reader stays open");
    // Row 1 holds the list's first row and the reader pane's title row.
    let rows = draw(&f);
    assert!(
        rows[1].contains("The thread could not be read: disk I/O error"),
        "{rows:#?}"
    );
    assert!(
        f.screen
            .status_line()
            .contains("The thread could not be read")
    );
}

#[tokio::test]
async fn a_fetch_that_gets_through_after_a_lost_watch_says_the_daemon_is_back() {
    let mut f = fixture(3).await;
    let lost = MailEvent::Daemon(DaemonEvent::Lost(OpError::internal("gone")));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(lost)));
    settle(&mut f.screen, effects).await;
    assert!(f.screen.status_line().contains("daemon unreachable"));
    // Opening a thread fetches its body; the daemon answers this time.
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(!line.contains("unreachable"), "{line}");
    assert!(
        line.contains("daemon reachable again (Ctrl-r resumes live updates)"),
        "{line}"
    );
    let effects = f.screen.update(Msg::Key(ctrl('r')));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        f.fake.calls.lock().unwrap().last(),
        Some(&FakeCall::Watch),
        "Ctrl-r subscribes again"
    );
    assert!(
        !f.screen.status_line().contains("daemon"),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn a_fetch_that_finds_the_daemon_gone_again_after_reachable_says_unreachable() {
    let mut f = fixture(3).await;
    let lost = MailEvent::Daemon(DaemonEvent::Lost(OpError::internal("gone")));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(lost)));
    settle(&mut f.screen, effects).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.status_line().contains("daemon reachable again"));
    // The next fetch fails the way a closed connection does.
    let closed = OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the daemon closed the connection",
    );
    let failed = MailEvent::Bodies {
        thread_id: 0,
        result: Err(closed),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(failed)));
    settle(&mut f.screen, effects).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("daemon unreachable (Ctrl-r retries)"),
        "{line}"
    );
    assert!(!line.contains("reachable again"), "{line}");
}

#[tokio::test]
async fn an_html_answer_for_a_message_outside_the_open_thread_is_dropped() {
    let mut f = fixture(3).await;
    let (opener, opened) = recording_opener();
    f.screen.set_opener(opener);
    press(&mut f, KeyCode::Enter).await;
    let own = f.screen.reader.as_ref().unwrap().detail().messages[0].id;
    // `o` was pressed in another thread (wm1's) before this one was opened.
    let other: i64 = writer(&f)
        .conn()
        .query_row("SELECT id FROM messages WHERE remote_id = 'wm1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let html = |message_id| MailEvent::Html {
        message_id,
        result: Ok(Some(HtmlBody::Html("<p>hi</p>".into()))),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(html(other))));
    settle(&mut f.screen, effects).await;
    assert!(
        opened.lock().unwrap().is_empty(),
        "another thread's message"
    );
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(html(own))));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        opened.lock().unwrap().len(),
        1,
        "the open thread's own message"
    );
}

#[tokio::test]
async fn a_reread_of_the_open_thread_leaves_the_focus_alone() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.focus, Focus::Reader, "opening a thread focuses it");
    // No key moves the focus off an open reader today (the palette closes it first); a
    // later view may, so the rule is pinned by setting the focus directly.
    f.screen.focus = Focus::List;
    let reads = f.screen.ctx.store.queries();
    store_changed(&mut f).await;
    assert!(
        f.screen.ctx.store.queries() > reads,
        "the thread was read again"
    );
    assert!(f.screen.reader.is_some());
    assert_eq!(f.screen.focus, Focus::List);
}

#[tokio::test]
async fn another_jobs_panic_leaves_the_refresh_in_flight() {
    let mut f = fixture(3).await;
    let status = status("work", "w@example.com", SyncPhase::Backfill);
    let progress = MailEvent::Daemon(DaemonEvent::Progress(Box::new(status)));
    let mut timer = f.screen.update(Msg::Task(TaskResult::Mail(progress)));
    let fired = run_job(timer.remove(0)).await;
    let mut refresh = feed(&mut f, fired);
    assert!(f.screen.refreshing.is_some(), "a refresh is in flight");
    // Some other job (an HTML read, a body fetch) panics meanwhile.
    f.screen
        .update(Msg::Task(TaskResult::JobPanicked("boom".into())));
    assert!(
        f.screen.refreshing.is_some(),
        "not that job's refresh to end"
    );
    assert!(f.screen.status_line().contains("internal error: boom"));
    let answers = run_job(refresh.remove(0)).await;
    assert!(
        matches!(
            answers.last(),
            Some(TaskResult::Mail(MailEvent::RefreshEnded { .. }))
        ),
        "{answers:?}"
    );
    feed(&mut f, answers);
    assert!(
        f.screen.refreshing.is_none(),
        "the refresh's own end ends it"
    );
}
