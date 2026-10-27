//! Bulk selection through the mail screen with the fake daemon over a seeded store: `* a`
//! against the scope and the live filter and its cap, `* f` and the addresses it refuses, the
//! `v` range and `Esc`'s precedence, the chunked `action.apply` and `action.undo`, the
//! confirmation and its threshold, help and the palette, and the snapshots.

use std::sync::{Arc, Mutex};

use rail_core::config::{AccountKind, Icons};
use rail_core::ops::{
    Action as MailAction, ActionApplyResponse, ActionUndoResponse, M365_LABELS, client_token,
};
use rail_core::provider::{Envelope, Mailbox};
use rail_core::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};
use rail_core::store::view::ThreadKey;

use super::super::bulk::{MAX_SELECTION, needs_confirm, question, title};
use super::super::selection::Picked;
use super::*;
use crate::tui::data::ApplyCall;
use crate::tui::modals::Target;
use crate::tui::test_util::render_lines;

mod chunks;
mod late_answers;
mod shift_range;

/// Press a two-key chord, e.g. `chord(f, '*', 'a')`.
async fn chord(f: &mut Fixture, first: char, second: char) {
    press(f, KeyCode::Char(first)).await;
    press(f, KeyCode::Char(second)).await;
}

/// Type `text` into the `/` filter line and commit it with Enter.
async fn filter_line(f: &mut Fixture, text: &str) {
    press(f, KeyCode::Char('/')).await;
    for c in text.chars() {
        press(f, KeyCode::Char(c)).await;
    }
    press(f, KeyCode::Enter).await;
}

/// Feed an event as the daemon or a read would.
async fn feed(f: &mut Fixture, event: MailEvent) {
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
}

/// The `Undo` calls the fake daemon has taken.
fn undos(f: &Fixture) -> Vec<FakeCall> {
    f.fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, FakeCall::Undo { .. }))
        .cloned()
        .collect()
}

/// Every `ApplyCall` the fake took, cloned before it answers: the only way to see `seq` and
/// `client_token`, which `FakeCall::Apply` does not record. Each call is also applied to the
/// store as the daemon would — a copy of `tests/actions.rs`'s `applying`, which is private to
/// `mod actions`. With `fail_threads`, the call whose `threads` equal it answers `Err`.
fn recording(f: &Fixture, fail_threads: Option<Vec<i64>>) -> Arc<Mutex<Vec<ApplyCall>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let db = f.paths.db_file();
    *f.fake.on_apply.lock().unwrap() = Some(Box::new(move |call: &ApplyCall| {
        log.lock().unwrap().push(call.clone());
        if fail_threads.as_ref().is_some_and(|t| *t == call.threads) {
            return Err(OpError::not_found("the daemon said no"));
        }
        let mut store = Store::open(&db).unwrap();
        let account: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM accounts WHERE name = ?1",
                [&call.account],
                |r| r.get(0),
            )
            .unwrap();
        let new = NewAction {
            account_id: account,
            action: call.action.clone(),
            threads: call.threads.clone(),
            not_before: i64::MAX,
            client_token: client_token(),
        };
        let OutboxOutcome::Enqueued { rows, applied } =
            store.outbox_write(&OutboxWrite::Enqueue(new), 0).unwrap()
        else {
            panic!("the enqueue was refused")
        };
        Ok(ActionApplyResponse { rows, applied })
    }));
    seen
}

/// Put `ids` of the account with this store id into the selection through `Selection::add`, as
/// a bulk select does; true when the cap dropped any.
fn select(f: &mut Fixture, account_id: i64, ids: &[i64]) -> bool {
    let targets: Vec<(i64, Picked)> = ids
        .iter()
        .map(|id| {
            (
                *id,
                Picked {
                    account_id,
                    starred: false,
                    unread: true,
                },
            )
        })
        .collect();
    f.screen.actions.selection.add(&targets)
}

/// The selected thread ids, ascending.
fn selected_ids(f: &Fixture) -> Vec<i64> {
    f.screen
        .actions
        .selection
        .targets()
        .iter()
        .map(|(id, _)| *id)
        .collect()
}

/// The thread ids of the account with this store id, ascending.
fn ids_of(f: &Fixture, account_id: i64) -> Vec<i64> {
    let store = writer(f);
    let mut statement = store
        .conn()
        .prepare("SELECT id FROM threads WHERE account_id = ?1 ORDER BY id")
        .unwrap();
    statement
        .query_map([account_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<i64>, _>>()
        .unwrap()
}

/// The name of the account the thread belongs to.
fn account_of(f: &Fixture, thread_id: i64) -> String {
    writer(f)
        .conn()
        .query_row(
            "SELECT a.name FROM accounts a JOIN threads t ON t.account_id = a.id WHERE t.id = ?1",
            [thread_id],
            |r| r.get(0),
        )
        .unwrap()
}

/// The store id of the thread whose remote id is `remote`.
fn thread_of(f: &Fixture, remote: &str) -> i64 {
    writer(f)
        .conn()
        .query_row(
            "SELECT id FROM threads WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

/// `mail_envelope` from this sender, in its own thread `{id}t`.
fn from_sender(id: &str, ms: i64, name: Option<&str>, address: &str) -> Envelope {
    Envelope {
        from: vec![Mailbox {
            name: name.map(str::to_string),
            address: address.to_string(),
        }],
        ..mail_envelope(id, &format!("{id}t"), ms)
    }
}

/// Add `upserts` to `work` as the daemon would.
fn apply_batch(f: &Fixture, upserts: Vec<Envelope>) {
    let mut store = writer(f);
    let batch = Batch {
        upserts,
        ..Batch::new(1)
    };
    store
        .apply(&batch, BASE_MS / 1000)
        .expect("apply the batch");
}

/// The open confirmation's body lines.
fn confirm_lines(f: &Fixture) -> Vec<String> {
    match &f.screen.modal {
        Some(Modal::Confirm(confirm)) => confirm.lines().to_vec(),
        _ => panic!("no confirmation is open"),
    }
}

/// The confirmation's box title is drawn, as `modals::boxed` draws it at 90 columns.
fn shows_title(f: &Fixture, head: &str) -> bool {
    render_lines(90, 24, |frame| f.screen.view(frame, &Theme::default()))
        .iter()
        .any(|row| row.contains(&format!("┌ {head} ")))
}

/// 1..=`n` as thread ids.
fn count_ids(n: usize) -> Vec<i64> {
    (1..=i64::try_from(n).unwrap()).collect()
}

#[tokio::test]
async fn select_all_selects_every_matching_thread() {
    let mut f = fixture(60).await;
    chord(&mut f, '*', 'a').await;
    assert_eq!(
        f.screen.actions.selection.len(),
        62,
        "60 work and 2 home inbox threads"
    );
    assert!(
        f.screen.header_left().contains("· 62 selected"),
        "the header counts the 62 selected threads"
    );
}

#[tokio::test]
async fn select_all_honours_the_live_filter() {
    let mut f = fixture(60).await;
    // Every seeded envelope is unread, so leave only the three newest messages so.
    writer(&f)
        .conn()
        .execute(
            "UPDATE messages SET is_read = 1 WHERE id NOT IN \
             (SELECT id FROM messages ORDER BY internal_date DESC, id DESC LIMIT 3)",
            [],
        )
        .unwrap();
    store_changed(&mut f).await;
    filter_line(&mut f, "is:unread").await;
    let unread = thread_ids(&f);
    assert_eq!(unread.len(), 3, "the filtered list: {unread:?}");
    chord(&mut f, '*', 'a').await;
    let mut wanted = unread;
    wanted.sort_unstable();
    assert_eq!(selected_ids(&f), wanted);
    assert!(
        f.screen.header_left().contains("· 3 selected"),
        "the header counts the 3 selected threads"
    );
}

#[tokio::test]
async fn a_capped_select_all_says_more_match() {
    let mut f = fixture(3).await;
    chord(&mut f, '*', 'a').await;
    press(&mut f, KeyCode::Esc).await;
    assert!(
        f.screen.actions.selection.is_empty(),
        "the five real matches are cleared"
    );
    // One more than the cap, newest first, as `Store::matching_targets` answers.
    let answer: Vec<(ThreadKey, i64, bool, bool)> = (0..=MAX_SELECTION)
        .map(|i| {
            let id = i64::try_from(MAX_SELECTION - i).unwrap() + 1;
            (
                ThreadKey {
                    last_date: id,
                    thread_id: id,
                },
                1,
                false,
                true,
            )
        })
        .collect();
    let dropped = answer[MAX_SELECTION].0.thread_id;
    let kept: Vec<(i64, Picked)> = (2..=i64::try_from(MAX_SELECTION).unwrap() + 1)
        .map(|id| {
            (
                id,
                Picked {
                    account_id: 1,
                    starred: false,
                    unread: true,
                },
            )
        })
        .collect();
    let seq = f.screen.bulk.latest_match;
    feed(
        &mut f,
        MailEvent::Matched {
            seq,
            result: Ok(answer),
        },
    )
    .await;
    assert_eq!(f.screen.actions.selection.len(), MAX_SELECTION);
    assert!(
        !f.screen.actions.selection.contains(dropped),
        "the oldest tuple is the one left out"
    );
    // The selection holds nothing but the four fields `matching_targets` answered: no subject
    // and no participant reaches it, even at 10,000 threads.
    assert_eq!(f.screen.actions.selection.targets(), kept);
    assert!(
        f.screen
            .header_left()
            .ends_with("· 10,000 selected (more match)"),
        "the header says 10,000 selected and that more match"
    );
}

#[tokio::test]
async fn select_all_with_no_match_says_so() {
    let mut f = fixture(3).await;
    filter_line(&mut f, "zzzznothing").await;
    assert!(
        f.screen.list.rows().is_empty(),
        "nothing matches the filter"
    );
    chord(&mut f, '*', 'a').await;
    assert!(f.screen.actions.selection.is_empty());
    assert!(
        f.screen.status_line().contains("Nothing matches."),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn select_sender_filters_and_selects_that_sender() {
    let mut f = fixture(320).await;
    // Three threads from dana, the last of them past `MAX_WINDOW`, and the three senders the
    // exact-address form must leave out.
    apply_batch(
        &f,
        vec![
            from_sender("d0", BASE_MS + 30_000_000, Some("Dana"), "dana@example.com"),
            from_sender("d1", BASE_MS + 29_000_000, None, "dana@example.com"),
            from_sender("d2", BASE_MS - 86_400_000, Some("Dana"), "dana@example.com"),
            from_sender("n0", BASE_MS + 28_000_000, None, "xdana@example.com"),
            from_sender("n1", BASE_MS + 27_000_000, None, "dana@example.com.evil"),
            from_sender(
                "n2",
                BASE_MS + 26_000_000,
                Some("dana@example.com"),
                "other@evil.example",
            ),
        ],
    );
    store_changed(&mut f).await;
    press(&mut f, KeyCode::Home).await;
    let cursor = f.screen.list.selected().unwrap().key.thread_id;
    assert_eq!(
        cursor,
        thread_of(&f, "d0t"),
        "the cursor is on a dana thread"
    );
    let mut wanted = ["d0t", "d1t", "d2t"].map(|t| thread_of(&f, t)).to_vec();
    wanted.sort_unstable();
    assert!(
        !thread_ids(&f).contains(&thread_of(&f, "d2t")),
        "the oldest dana thread is outside the drawn window"
    );
    chord(&mut f, '*', 'f').await;
    assert_eq!(f.screen.slot_text(), "/ from:<dana@example.com>");
    assert_eq!(selected_ids(&f), wanted);
}

#[tokio::test]
async fn select_sender_with_no_sender_says_so() {
    let mut f = fixture(3).await;
    // Both providers drop an address-less `From`, which reads as `from_json = '[]'`.
    apply_batch(
        &f,
        vec![Envelope {
            from: Vec::new(),
            ..mail_envelope("x0", "x0t", BASE_MS + 30_000_000)
        }],
    );
    store_changed(&mut f).await;
    press(&mut f, KeyCode::Home).await;
    let slot = f.screen.slot_text();
    let queries = f.screen.ctx.store.queries();
    chord(&mut f, '*', 'f').await;
    assert_eq!(f.screen.slot_text(), slot, "the filter text is left alone");
    assert_eq!(f.screen.actions.selection.len(), 0);
    assert!(
        f.screen
            .status_line()
            .contains("This thread has no sender to select by."),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(
        f.screen.ctx.store.queries(),
        queries + 1,
        "the sender read, and no matching read after it"
    );
}

#[tokio::test]
async fn select_sender_refuses_an_unfilterable_address() {
    let mut f = fixture(3).await;
    // Legal, and `tokens()` splits it at the space: the round trip cannot carry it.
    apply_batch(
        &f,
        vec![from_sender(
            "q0",
            BASE_MS + 30_000_000,
            None,
            "\"dana reyes\"@example.com",
        )],
    );
    store_changed(&mut f).await;
    press(&mut f, KeyCode::Home).await;
    let slot = f.screen.slot_text();
    let queries = f.screen.ctx.store.queries();
    chord(&mut f, '*', 'f').await;
    assert_eq!(f.screen.slot_text(), slot, "the filter text is left alone");
    assert_eq!(f.screen.actions.selection.len(), 0);
    assert!(
        f.screen
            .status_line()
            .contains("This thread has no sender to select by."),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.ctx.store.queries(), queries + 1);
}

#[tokio::test]
async fn a_range_selects_the_rows_it_covers() {
    let mut f = fixture(3).await;
    let rows = thread_ids(&f);
    press(&mut f, KeyCode::Char('v')).await;
    assert!(f.screen.bulk_range_on());
    assert!(
        f.screen.status_line().contains("range from here"),
        "{}",
        f.screen.status_line()
    );
    // A notice replaces the hints, so the lasting cue is read once the notice has gone.
    expire_notice(&mut f).await;
    let theme = Theme::default();
    let hints = f.screen.bar(&theme).hints;
    let first = hints.first().expect("the bar has hints");
    assert_eq!(
        (first.key.as_str(), first.word.as_str()),
        ("Esc", "range"),
        "the range hint is first, so `fit_hints` never trims it"
    );
    let last = hints.last().expect("the bar has hints");
    assert_eq!((last.key.as_str(), last.word.as_str()), ("?", "help"));
    let stacked = render_lines(90, 24, |frame| f.screen.view(frame, &theme));
    assert!(
        stacked[23].starts_with("Esc range  Enter open  j/k move  r reply  c compose  ? help"),
        "{}",
        stacked[23]
    );
    let boxed = render_lines(150, 40, |frame| f.screen.view(frame, &theme));
    assert!(
        boxed[38].contains("│ Esc │ range │ Enter │ open"),
        "{}",
        boxed[38]
    );
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    press(&mut f, KeyCode::Char('v')).await;
    assert!(!f.screen.bulk_range_on());
    let mut wanted = rows[0..3].to_vec();
    wanted.sort_unstable();
    assert_eq!(selected_ids(&f), wanted);
}

#[tokio::test]
async fn esc_ends_the_range_then_clears_the_selection() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    press(&mut f, KeyCode::Char('v')).await;
    press_n(&mut f, KeyCode::Char('j'), 3).await;
    press(&mut f, KeyCode::Esc).await;
    assert!(!f.screen.bulk_range_on());
    let mut wanted = rows[2..=5].to_vec();
    wanted.sort_unstable();
    assert_eq!(selected_ids(&f), wanted, "the range is kept");
    press(&mut f, KeyCode::Esc).await;
    assert!(f.screen.actions.selection.is_empty());
}

#[tokio::test]
async fn a_lost_range_anchor_says_so() {
    let mut f = fixture(3).await;
    let anchor = f.screen.list.selected().unwrap().key.thread_id;
    press(&mut f, KeyCode::Char('v')).await;
    let mut store = writer(&f);
    store
        .apply(
            &Batch {
                deletes: vec!["wm2".into()],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .expect("delete the anchor's message");
    drop(store);
    store_changed(&mut f).await;
    assert!(
        !thread_ids(&f).contains(&anchor),
        "the anchor left the window"
    );
    press(&mut f, KeyCode::Char('v')).await;
    assert!(f.screen.actions.selection.is_empty(), "nothing is added");
    assert!(!f.screen.bulk_range_on());
    assert!(
        f.screen
            .status_line()
            .contains("The range start is no longer in the list."),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn the_filter_line_wins_esc() {
    let mut f = fixture(3).await;
    filter_line(&mut f, "is:unread").await;
    press(&mut f, KeyCode::Char('v')).await;
    assert!(f.screen.bulk_range_on());
    press(&mut f, KeyCode::Char('/')).await;
    press(&mut f, KeyCode::Esc).await;
    assert!(f.screen.filter.is_empty(), "the live filter is cleared");
    assert_eq!(f.screen.slot_text(), "/ Search");
    assert!(f.screen.bulk_range_on(), "range mode is untouched");
}

#[tokio::test]
async fn archive_over_the_threshold_asks_first() {
    let mut f = fixture(60).await;
    let work = ids_of(&f, 1);
    select(&mut f, 1, &work[..60]);
    press(&mut f, KeyCode::Char('e')).await;
    assert!(applies(&f).is_empty(), "nothing before the answer");
    assert_eq!(confirm_lines(&f), ["Archive 60 threads?"]);
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(applies(&f).len(), 1);
}

#[tokio::test]
async fn answering_no_sends_nothing() {
    let mut f = fixture(60).await;
    let work = ids_of(&f, 1);
    select(&mut f, 1, &work[..60]);
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('n')).await;
    assert_eq!(applies(&f), Vec::<FakeCall>::new());
    assert_eq!(f.screen.actions.selection.len(), 60, "the selection stays");
    press(&mut f, KeyCode::Char('z')).await;
    assert!(
        f.screen.status_line().contains("Nothing to undo."),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn the_threshold_is_exclusive_and_zero_never_asks() {
    let mut f = fixture(60).await;
    let work = ids_of(&f, 1);
    select(&mut f, 1, &work[..50]);
    press(&mut f, KeyCode::Char('e')).await;
    assert!(f.screen.modal.is_none(), "the default does not ask at 50");
    assert_eq!(applies(&f).len(), 1);

    let mut f = fixture(60).await;
    f.screen.ctx.bulk_confirm = 0;
    let work = ids_of(&f, 1);
    select(&mut f, 1, &work[..60]);
    press(&mut f, KeyCode::Char('e')).await;
    assert!(f.screen.modal.is_none(), "`0` never asks");
    assert_eq!(applies(&f).len(), 1);
}

#[tokio::test]
async fn the_question_names_the_action_and_the_count() {
    let mut f = fixture(3).await;
    select(&mut f, 1, &count_ids(1234));
    press(&mut f, KeyCode::Char('s')).await;
    assert_eq!(confirm_lines(&f), ["Star 1,234 threads?"]);
    assert!(shows_title(&f, "Star?"));
    press(&mut f, KeyCode::Char('n')).await;
    // Every `Picked` is unread, so `u` means mark read.
    press(&mut f, KeyCode::Char('u')).await;
    assert_eq!(confirm_lines(&f), ["Mark 1,234 threads read?"]);
    assert!(shows_title(&f, "Mark read?"));
}

#[tokio::test]
async fn trash_always_asks_and_counts_with_commas() {
    let mut f = fixture(3).await;
    select(&mut f, 1, &count_ids(1234));
    press(&mut f, KeyCode::Char('d')).await;
    let lines = confirm_lines(&f);
    assert_eq!(lines[0], "Trash 1,234 threads?");
    assert!(lines.len() > 1, "each provider's note follows: {lines:?}");
    press(&mut f, KeyCode::Char('y')).await;
    assert!(
        f.screen.status_line().contains("trashed 1,234 · z undoes"),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn labels_are_still_refused_across_accounts_and_on_m365() {
    let mut f = fixture(450).await;
    let work = ids_of(&f, 1);
    let home = ids_of(&f, 2);
    select(&mut f, 1, &work[..298]);
    select(&mut f, 2, &home);
    assert_eq!(f.screen.actions.selection.len(), 300);
    press(&mut f, KeyCode::Char('l')).await;
    assert!(f.screen.modal.is_none());
    assert_eq!(applies(&f), Vec::<FakeCall>::new());
    assert!(
        f.screen
            .status_line()
            .contains("Labels belong to one account: select threads from one account."),
        "{}",
        f.screen.status_line()
    );

    let mut f = fixture(450).await;
    f.screen.accounts[1].kind = AccountKind::M365;
    let home = ids_of(&f, 2);
    select(&mut f, 2, &home);
    press(&mut f, KeyCode::Char('l')).await;
    assert!(f.screen.modal.is_none());
    assert!(
        f.screen.status_line().contains(M365_LABELS),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn bulk_keys_do_nothing_in_the_reader() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.reader().is_some());
    let queries = f.screen.ctx.store.queries();
    let targets = f.screen.actions.selection.targets();
    let seq = f.screen.bulk.latest_match;
    let calls = f.fake.calls.lock().unwrap().clone();
    // Called rather than typed: `*` starts no chord in the Reader, so `a` would reach
    // `reader.attachments` and `f` `list.forward` — keymap resolution, not the bulk guards.
    for action in [
        Action::ListSelectAll,
        Action::ListSelectSender,
        Action::ListSelectRange,
    ] {
        let effects = f.screen.act(action);
        settle(&mut f.screen, effects).await;
    }
    assert_eq!(f.screen.ctx.store.queries(), queries, "no store read");
    assert_eq!(f.screen.actions.selection.targets(), targets);
    assert_eq!(f.screen.bulk.latest_match, seq);
    assert_eq!(*f.fake.calls.lock().unwrap(), calls, "no daemon call");
    assert!(!f.screen.bulk_range_on());
}

#[test]
fn the_question_text_is_fixed() {
    let cases = [
        (MailAction::Archive {}, "Archive?", "Archive 1,234 threads?"),
        (
            MailAction::Star { on: true },
            "Star?",
            "Star 1,234 threads?",
        ),
        (
            MailAction::Star { on: false },
            "Unstar?",
            "Unstar 1,234 threads?",
        ),
        (
            MailAction::Read { on: true },
            "Mark read?",
            "Mark 1,234 threads read?",
        ),
        (
            MailAction::Read { on: false },
            "Mark unread?",
            "Mark 1,234 threads unread?",
        ),
    ];
    for (action, head, body) in cases {
        assert_eq!(title(&action), head);
        assert_eq!(question(&action, 1234), body);
    }
    let archive = MailAction::Archive {};
    assert!(
        !needs_confirm(&archive, 50, 50),
        "the threshold is exclusive"
    );
    assert!(needs_confirm(&archive, 51, 50));
    assert!(!needs_confirm(&archive, MAX_SELECTION, 0), "`0` never asks");
}

#[tokio::test]
async fn snapshot_bulk_confirm_and_header_at_both_sizes_and_icon_sets() {
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        for (cols, rows) in [(90u16, 24u16), (150, 40)] {
            let mut f = fixture(3).await;
            f.screen.ctx.icons = icons;
            select(&mut f, 1, &count_ids(1234));
            press(&mut f, KeyCode::Char('e')).await;
            let shot = crate::tui::test_util::snapshot(cols, rows, |frame| {
                f.screen.view(frame, &Theme::default());
            });
            insta::assert_snapshot!(format!("bulk_confirm_{name}_{cols}x{rows}"), shot);

            let mut f = fixture(3).await;
            f.screen.ctx.icons = icons;
            select(&mut f, 1, &count_ids(MAX_SELECTION));
            f.screen.bulk.more = true;
            press(&mut f, KeyCode::Char('v')).await;
            expire_notice(&mut f).await;
            let shot = crate::tui::test_util::snapshot(cols, rows, |frame| {
                f.screen.view(frame, &Theme::default());
            });
            insta::assert_snapshot!(format!("bulk_header_{name}_{cols}x{rows}"), shot);
        }
    }
}

#[tokio::test]
async fn the_header_and_the_confirmation_survive_a_tiny_terminal() {
    let mut f = fixture(3).await;
    select(&mut f, 1, &count_ids(MAX_SELECTION));
    f.screen.bulk.more = true;
    press(&mut f, KeyCode::Char('e')).await;
    assert!(matches!(f.screen.modal, Some(Modal::Confirm(_))));
    for (cols, rows) in [(0u16, 24u16), (1, 24), (90, 3), (2, 1)] {
        let drawn = render_lines(cols, rows, |frame| f.screen.view(frame, &Theme::default()));
        assert_eq!(drawn.len(), usize::from(rows), "{cols}x{rows}");
    }
}

#[tokio::test]
async fn the_new_actions_reach_help_and_the_palette() {
    let mut f = fixture(3).await;
    let wanted = [
        (
            Action::ListSelectAll,
            "* a",
            "Select every thread this mailbox and filter match",
        ),
        (
            Action::ListSelectSender,
            "* f",
            "Select every thread from this sender",
        ),
        (Action::ListSelectRange, "v", "Select a range of threads"),
    ];
    press(&mut f, KeyCode::Char('?')).await;
    let Some(Modal::Help(help)) = &f.screen.modal else {
        panic!("no help is open")
    };
    let text = help.text();
    for (_, keys, description) in wanted {
        let row = text
            .lines()
            .find(|line| line.contains(description))
            .unwrap_or_else(|| panic!("{description} is not in `?`:\n{text}"));
        assert!(row.contains(keys), "{row}");
    }
    press(&mut f, KeyCode::Esc).await;
    let entries = f.screen.palette_entries();
    for (action, keys, description) in wanted {
        let entry = entries
            .iter()
            .find(|entry| entry.target == Target::Action(action))
            .unwrap_or_else(|| panic!("{description} is not in `:`"));
        assert_eq!(entry.text, description);
        assert_eq!(entry.key, keys);
    }
}
