//! Saved searches on the mail screen: `S` and the name prompt, the `SEARCHES` sidebar section
//! and its counts, the palette, opening one like a mailbox, and `:search-counts`.
//! AC-1 … AC-11, AC-13b, AC-19; `r` and `d` (AC-14 … AC-18) are in `edits`.

use rail_core::config::SavedSearch;
use rail_core::ops::SavedSearchListResponse;
use rail_core::provider::{Envelope, Mailbox};

use super::compose::type_text;
use super::sidebar::sidebar_to;
use super::*;
use crate::tui::modals::{PaletteEntry, Target};
use crate::tui::screens::mail::sidebar_tree::{RowKey, RowKind};

mod edits;
mod stale;

/// A `Fixture` whose config holds `searches` (name, query) and whose store holds `envelopes`
/// (account index, envelope), started and settled. `seed_store` puts every `mail_envelope` in
/// INBOX, so a test needing another folder or a read thread hands its own envelopes in. Named
/// `saved_fixture`, not `fixture_with`, which `tests.rs:63` already has; it is that function's
/// body with the envelopes and the two context fields injected.
async fn saved_fixture(envelopes: &[(usize, Envelope)], searches: &[(&str, &str)]) -> Fixture {
    let saved: Vec<SavedSearch> = searches
        .iter()
        .map(|(n, q)| SavedSearch {
            name: (*n).into(),
            query: (*q).into(),
        })
        .collect();
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
    fake.saved.lock().unwrap().clone_from(&saved);
    let ctx = MailContext {
        saved_searches: saved,
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

/// A one-message thread from Dana in `folder`, read or unread.
fn dana(id: &str, thread: &str, ms: i64, folder: &str, read: bool) -> Envelope {
    Envelope {
        from: vec![Mailbox {
            name: Some("Dana Reyes".into()),
            address: "dana@corp.example".into(),
        }],
        folders: vec![folder.to_string()],
        is_read: read,
        ..crate::tui::test_util::mail_envelope(id, thread, ms)
    }
}

/// `work`'s three inbox threads and `home`'s two: `fixture(3)`'s five.
fn five_threads() -> Vec<(usize, Envelope)> {
    let mut all: Vec<_> = inbox_threads("w", 3).into_iter().map(|e| (0, e)).collect();
    all.extend(inbox_threads("h", 2).into_iter().map(|e| (1, e)));
    all
}

/// The two searches most of these tests save.
const TWO: [(&str, &str); 2] = [
    ("needs-reply", "from:dana is:unread"),
    ("big-attachments", "has:attachment"),
];

/// `f` drawn at `width` x `height`, rows joined, trailing spaces kept (cell-exact).
fn drawn(f: &Fixture, width: u16, height: u16) -> String {
    crate::tui::test_util::render_lines(width, height, |frame| {
        f.screen.view(frame, &Theme::default());
    })
    .join("\n")
}

/// `f` drawn at `width` x `height` as a snapshot (each row's trailing spaces trimmed).
fn shot(f: &Fixture, width: u16, height: u16) -> String {
    crate::tui::test_util::snapshot(width, height, |frame| {
        f.screen.view(frame, &Theme::default());
    })
}

/// Every `SavedSearchPut` / `SavedSearchRemove` the fake daemon took, in order.
fn saved_calls(f: &Fixture) -> Vec<FakeCall> {
    f.fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| {
            matches!(
                c,
                FakeCall::SavedSearchPut { .. } | FakeCall::SavedSearchRemove { .. }
            )
        })
        .cloned()
        .collect()
}

/// The open name prompt's refusal, `None` when none is shown or no prompt is open.
fn refusal(f: &Fixture) -> Option<String> {
    match &f.screen.modal {
        Some(Modal::Prompt(prompt)) => prompt.refusal().map(str::to_string),
        _ => None,
    }
}

/// FR-15's body line for `needs-reply`.
const REMOVE_LINE: &str =
    "Remove the saved search \"needs-reply\"? The mail it finds is not touched.";

/// The open confirmation's body lines.
fn confirm_lines(f: &Fixture) -> Vec<String> {
    match &f.screen.modal {
        Some(Modal::Confirm(confirm)) => confirm.lines().to_vec(),
        _ => panic!("no confirmation is open"),
    }
}

/// The thread subjects the list shows, newest first.
fn subjects(f: &Fixture) -> Vec<String> {
    f.screen
        .list
        .rows()
        .iter()
        .map(|r| r.subject.clone())
        .collect()
}

/// The thread ids the list shows, ascending: what a select-all over it must take.
fn listed_ids(f: &Fixture) -> Vec<i64> {
    let mut ids: Vec<i64> = f
        .screen
        .list
        .rows()
        .iter()
        .map(|r| r.key.thread_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// The selected thread ids, ascending (`Selection` keeps them in a `BTreeMap`), as
/// `tests/bulk.rs` reads them.
fn selected_ids(f: &Fixture) -> Vec<i64> {
    f.screen
        .actions
        .selection
        .targets()
        .iter()
        .map(|(id, _)| *id)
        .collect()
}

/// The `(label, count)` of every saved-search row, in sidebar order.
fn saved_rows(f: &Fixture) -> Vec<(String, Option<u64>)> {
    f.screen
        .sidebar
        .rows()
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Saved { .. }))
        .map(|r| (r.label.clone(), r.count))
        .collect()
}

/// The drawn sidebar slice of the row holding `label` at 90×24, trailing spaces trimmed —
/// what the user sees, whichever side of the draw FR-19's counts-off guard sits on: §6 slot 9
/// and §12 step 13 pin a draw guard, FR-19 also says `Store::saved_counts` is not run, and
/// `SidebarRow.count` alone cannot tell a guarded draw from a re-read list. Panics with the
/// whole frame when no sidebar row holds `label`, so the assertion is never vacuous. Callers
/// have been through `sidebar_to`, which presses `Tab` and so shows the sidebar at 90 columns
/// (`SIDEBAR_MIN_COLUMNS = 100`); the slice is the AC-17 idiom.
fn drawn_saved_row(f: &Fixture, label: &str) -> String {
    let shown = drawn(f, 90, 24);
    shown
        .lines()
        .map(|line| line.split('│').next().unwrap_or(line).trim_end())
        .find(|slice| slice.contains(label))
        .unwrap_or_else(|| panic!("no drawn sidebar row for {label:?}: {shown}"))
        .to_string()
}

/// `/` then `text` then Enter: the live filter in force, the line closed.
async fn set_filter(f: &mut Fixture, text: &str) {
    press(f, KeyCode::Char('/')).await;
    type_text(f, text).await;
    press(f, KeyCode::Enter).await;
}

/// `:` then `text` then Enter: the entry `text` matches, run.
async fn run_from_the_palette(f: &mut Fixture, text: &str) {
    press(f, KeyCode::Char(':')).await;
    type_text(f, text).await;
    press(f, KeyCode::Enter).await;
}

/// `sidebar_to` upwards: an open saved search leaves the cursor below the account rows.
async fn sidebar_up_to(f: &mut Fixture, account: Option<usize>, label: &str) {
    if f.screen.focus != Focus::Sidebar {
        press(f, KeyCode::Tab).await;
    }
    for _ in 0..f.screen.sidebar.rows().len() {
        let here = f
            .screen
            .sidebar
            .selected()
            .is_some_and(|r| r.label == label && r.account() == account);
        if here {
            return;
        }
        press(f, KeyCode::Char('k')).await;
    }
    panic!("no sidebar row {label:?} above the cursor");
}

/// `config.toml` as the user would have it.
fn write_config(f: &Fixture, text: &str) {
    let path = f.paths.config_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn config_text(f: &Fixture) -> String {
    std::fs::read_to_string(f.paths.config_file()).unwrap_or_default()
}

/// AC-1 (FR-1, FR-3): `S` names the filter in force and sends it verbatim.
#[tokio::test]
async fn s_saves_the_live_filter_under_a_name() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    set_filter(&mut f, "from:dana").await;
    press(&mut f, KeyCode::Char('S')).await;
    type_text(&mut f, "needs-reply").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchPut {
            name: "needs-reply".into(),
            query: Some("from:dana".into()),
            replaces: None,
        }]
    );
}

/// AC-2 (FR-2): with no filter there is nothing to save, and `S` says so.
#[tokio::test]
async fn s_without_a_filter_says_to_set_one() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    press(&mut f, KeyCode::Char('S')).await;
    assert!(f.screen.modal.is_none(), "nothing opened");
    assert!(
        f.screen
            .status_line()
            .contains("Set a filter with / first, then save it."),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(saved_calls(&f), Vec::new());
}

/// `S` on a filter that narrows nothing (only words the filter drops) is refused before the
/// prompt opens: saved, it would list nothing, which reads as a broken search.
#[tokio::test]
async fn s_on_a_filter_that_narrows_nothing_says_so() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    set_filter(&mut f, "* ( )").await;
    press(&mut f, KeyCode::Char('S')).await;
    assert!(f.screen.modal.is_none(), "nothing opened");
    assert!(
        f.screen
            .status_line()
            .contains("That filter narrows nothing; add a condition to save it."),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(saved_calls(&f), Vec::new());
}

/// AC-3 (FR-4): a name already saved is refused in the prompt, before any call.
#[tokio::test]
async fn a_clashing_name_is_refused_in_the_prompt() {
    let mut f = saved_fixture(&five_threads(), &TWO[..1]).await;
    set_filter(&mut f, "is:unread").await;
    press(&mut f, KeyCode::Char('S')).await;
    type_text(&mut f, "needs-reply").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        matches!(f.screen.modal, Some(Modal::Prompt(_))),
        "the prompt stays open"
    );
    assert_eq!(
        refusal(&f).as_deref(),
        Some("A saved search named \"needs-reply\" already exists.")
    );
    assert_eq!(saved_calls(&f), Vec::new());
}

/// AC-4 (FR-5): a bad name is refused in the prompt. Asserted through `refusal()`, not on the
/// frame: 64 cells against a 54-cell inner row, so only the wrapped drawing carries it whole.
#[tokio::test]
async fn a_bad_name_is_refused_in_the_prompt() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    set_filter(&mut f, "from:dana").await;
    press(&mut f, KeyCode::Char('S')).await;
    type_text(&mut f, "bad name!").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        refusal(&f).as_deref(),
        Some("A saved search name must be 1 to 40 letters, digits, '-' or '_'.")
    );
    assert_eq!(saved_calls(&f), Vec::new());
}

/// AC-5 (FR-1): `Esc` cancels and changes nothing, down to the frame.
#[tokio::test]
async fn esc_cancels_the_prompt() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    set_filter(&mut f, "from:dana").await;
    let before = drawn(&f, 90, 24);
    press(&mut f, KeyCode::Char('S')).await;
    type_text(&mut f, "x").await;
    press(&mut f, KeyCode::Esc).await;
    assert!(f.screen.modal.is_none());
    assert_eq!(saved_calls(&f), Vec::new());
    assert_eq!(drawn(&f, 90, 24), before);
}

/// AC-6 (FR-7, FR-8): a rule, a heading and one row per search in config order, each with its
/// unread count; the four snapshots pin the cells. Asserted on `rows()`, as `tests/sidebar.rs`
/// does: the stacked rule puts `│` after every row, so no frame line can equal `SEARCHES`.
#[tokio::test]
async fn the_sidebar_lists_saved_searches_with_unread_counts() {
    let envelopes = vec![
        (0, dana("d1", "dt1", BASE_MS, "INBOX", false)),
        (0, dana("d2", "dt2", BASE_MS + 60_000, "INBOX", true)),
        (0, mail_envelope("w1", "wt1", BASE_MS + 120_000)),
    ];
    let mut f = saved_fixture(&envelopes, &TWO).await;

    let rows = f.screen.sidebar.rows().to_vec();
    let at = rows
        .iter()
        .position(|r| r.key == RowKey::Heading("SEARCHES"))
        .unwrap_or_else(|| panic!("no SEARCHES heading: {rows:?}"));
    assert!(at > 0, "the section has a rule above it: {rows:?}");
    assert_eq!(rows[at - 1].key, RowKey::Rule(Some("SEARCHES".into())));
    assert!(!rows[at - 1].selectable(), "a rule is not selectable");
    assert!(!rows[at].selectable(), "a heading is not selectable");
    assert_eq!(
        saved_rows(&f),
        [
            ("needs-reply".to_string(), Some(1)),
            ("big-attachments".to_string(), None),
        ]
    );
    assert_eq!(
        rows[at + 1..].len(),
        2,
        "the rows are last: no trailing rule: {rows:?}"
    );
    assert!(
        rows[at + 1..]
            .iter()
            .all(|r| matches!(r.kind, RowKind::Saved { .. }) && r.selectable() && !r.foldable)
    );

    sidebar_snapshots(&mut f).await;
}

/// AC-6's four `saved_sidebar_*` snapshots, in the order that makes each one's fold state
/// unambiguous: the 150x40 pair with the accounts unfolded and `needs-reply` the open scope,
/// then the 90x24 pair with both folded — unfolded they leave three rows where the section
/// needs four.
async fn sidebar_snapshots(f: &mut Fixture) {
    sidebar_to(f, None, "needs-reply").await;
    press(f, KeyCode::Enter).await;
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        f.screen.ctx.icons = icons;
        insta::assert_snapshot!(format!("saved_sidebar_{name}_150x40"), shot(f, 150, 40));
    }
    f.screen.ctx.icons = Icons::Unicode;
    press(f, KeyCode::Char('b')).await;
    sidebar_up_to(f, Some(1), "home").await;
    press(f, KeyCode::Char(' ')).await;
    sidebar_up_to(f, Some(0), "work").await;
    press(f, KeyCode::Char(' ')).await;
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        f.screen.ctx.icons = icons;
        insta::assert_snapshot!(format!("saved_sidebar_{name}_90x24"), shot(f, 90, 24));
    }
}

/// AC-7: with none saved nothing says `SEARCHES`; `view_tests` guards the 11 snapshots.
#[tokio::test]
async fn no_saved_searches_draws_nothing_new() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    // Tab shows the sidebar at 90 columns too, so the assertion is not vacuous.
    press(&mut f, KeyCode::Tab).await;
    for icons in [Icons::Unicode, Icons::Ascii] {
        f.screen.ctx.icons = icons;
        for (width, height) in [(90, 24), (120, 30), (150, 40)] {
            let shown = drawn(&f, width, height);
            assert!(shown.contains("ACCOUNTS"), "the sidebar is drawn: {shown}");
            assert!(!shown.contains("SEARCHES"), "{shown}");
        }
    }
}

/// FR-9 and Q-5 for one saved search: its `Search: <name>` scope entry and the single
/// `Rename search:` / `Remove search:` pair carrying its index.
fn assert_search_entries(entries: &[PaletteEntry], index: usize, name: &str) {
    let scope = entries
        .iter()
        .find(|e| e.text == format!("Search: {name}"))
        .unwrap_or_else(|| panic!("no `Search: {name}` entry: {entries:?}"));
    assert_eq!(scope.search, format!("search {name}"));
    assert!(
        matches!(
            &scope.target,
            Target::Scope(ScopeChoice::Saved { index: i, name: n }) if *i == index && n == name
        ),
        "{scope:?}"
    );
    for (what, text, target) in [
        (
            "rename",
            format!("Rename search: {name}"),
            Target::RenameSearch {
                index,
                name: name.to_string(),
            },
        ),
        (
            "remove",
            format!("Remove search: {name}"),
            Target::RemoveSearch {
                index,
                name: name.to_string(),
            },
        ),
    ] {
        let found: Vec<_> = entries.iter().filter(|e| e.text == text).collect();
        assert_eq!(found.len(), 1, "one {what} entry for {name}: {entries:?}");
        assert_eq!(found[0].search, format!("{what}-search {name}"));
        assert_eq!(found[0].target, target);
    }
}

/// AC-8 (FR-9, Q-5): one scope entry per search, exactly one `save-search` (the `Action::ALL`
/// duplicate filtered out, the `compose` pattern), a rename/remove pair each, `search-counts`.
#[tokio::test]
async fn the_palette_lists_saved_searches_and_the_commands() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    set_filter(&mut f, "from:dana").await;
    press(&mut f, KeyCode::Char(':')).await;
    let entries = f.screen.palette_entries();

    assert_search_entries(&entries, 0, "needs-reply");
    assert_search_entries(&entries, 1, "big-attachments");
    let saves: Vec<_> = entries
        .iter()
        .filter(|e| e.text == "Save the filter as a search")
        .collect();
    assert_eq!(
        saves.len(),
        1,
        "exactly one, not one per route: {entries:?}"
    );
    assert_eq!(saves[0].key, "S");
    assert_eq!(saves[0].target, Target::SaveSearch);
    assert!(
        entries
            .iter()
            .any(|e| e.search.contains("search-counts") && e.target == Target::SearchCounts),
        "{entries:?}"
    );
}

/// AC-8b (FR-1): the palette reaches the same prompt, empty — `Enter` at once is refused.
#[tokio::test]
async fn the_palette_opens_the_save_prompt() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    set_filter(&mut f, "from:dana").await;
    run_from_the_palette(&mut f, "save-search").await;
    assert!(matches!(f.screen.modal, Some(Modal::Prompt(_))), "open");
    assert_eq!(refusal(&f), None, "nothing is refused yet");
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        refusal(&f).as_deref(),
        Some("A saved search name must be 1 to 40 letters, digits, '-' or '_'."),
        "it started empty"
    );
    assert_eq!(saved_calls(&f), Vec::new());
}

/// AC-9 (FR-2, Q-5): `save-search` needs a filter and the two others need a search;
/// `search-counts` is always there.
#[tokio::test]
async fn save_and_rename_are_offered_only_when_they_apply() {
    let mut f = saved_fixture(&five_threads(), &[]).await;
    press(&mut f, KeyCode::Char(':')).await;
    let entries = f.screen.palette_entries();
    for absent in ["save-search", "rename-search", "remove-search"] {
        assert!(
            !entries.iter().any(|e| e.search.contains(absent)),
            "{absent} is offered with nothing to use it on: {entries:?}"
        );
    }
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.search.contains("search-counts"))
            .count(),
        1,
        "{entries:?}"
    );
}

/// AC-10 (FR-10/11): like a mailbox, and `Scope::All` reaches `seed_store`'s `Label_1`.
#[tokio::test]
async fn opening_a_saved_search_shows_its_threads_like_a_mailbox() {
    let envelopes = vec![
        (0, dana("d1", "dt1", BASE_MS, "Label_1", false)),
        (0, dana("d2", "dt2", BASE_MS + 60_000, "Label_1", false)),
        (0, mail_envelope("w1", "wt1", BASE_MS + 120_000)),
        (0, mail_envelope("w2", "wt2", BASE_MS + 180_000)),
        (1, mail_envelope("h1", "ht1", BASE_MS + 240_000)),
    ];
    let mut f = saved_fixture(&envelopes, &[("dana", "from:dana")]).await;
    sidebar_to(&mut f, None, "dana").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        f.screen
            .header_left()
            .starts_with("▍Rail │ Search: dana · 2 unread"),
        "the header titles the open saved search and counts its 2 unread"
    );
    assert_eq!(subjects(&f), ["subject d2", "subject d1"]);
}

/// AC-11 (FR-12): `/` ANDs on top; `Esc` on the **reopened** line drops only it (closed, it
/// runs `list.clear_selection`), so the reopen is required, not cosmetic.
#[tokio::test]
async fn the_live_filter_ands_on_top_and_esc_clears_only_it() {
    let envelopes = vec![
        (0, dana("d1", "dt1", BASE_MS, "Label_1", false)),
        (0, dana("d2", "dt2", BASE_MS + 60_000, "Label_1", true)),
        (0, mail_envelope("w1", "wt1", BASE_MS + 120_000)),
    ];
    let mut f = saved_fixture(&envelopes, &[("dana", "from:dana")]).await;
    sidebar_to(&mut f, None, "dana").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(subjects(&f), ["subject d2", "subject d1"]);
    set_filter(&mut f, "is:unread").await;
    assert_eq!(subjects(&f), ["subject d1"]);
    press(&mut f, KeyCode::Char('/')).await;
    press(&mut f, KeyCode::Esc).await;
    assert_eq!(subjects(&f), ["subject d2", "subject d1"]);
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Saved { name, .. } if name == "dana"),
        "{:?}",
        f.screen.scope
    );
    assert!(
        f.screen
            .sidebar
            .rows()
            .iter()
            .any(|r| r.key == RowKey::Saved("dana".into()))
    );
}

/// AC-13b (FR-13): a saved `account:` narrows list and count; another one intersects to none.
#[tokio::test]
async fn account_inside_a_saved_query_narrows_the_list_and_the_count() {
    let envelopes = vec![
        (0, dana("dw", "dwt", BASE_MS, "INBOX", false)),
        (1, dana("dh", "dht", BASE_MS + 60_000, "INBOX", false)),
    ];
    let mut f = saved_fixture(&envelopes, &[("dana-home", "account:home from:dana")]).await;
    assert_eq!(saved_rows(&f), [("dana-home".to_string(), Some(1))]);
    sidebar_to(&mut f, None, "dana-home").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(subjects(&f), ["subject dh"]);
    set_filter(&mut f, "account:work").await;
    assert!(subjects(&f).is_empty(), "{:?}", subjects(&f));
}

/// Plan C.4 (FR-10, FR-12): `* a` inside an open saved search selects exactly the rows it
/// lists — the header's count and the ids — and never a thread the search leaves out. The
/// select-all and the list read through the one `ListQuery` `list_query` builds
/// (`data/store_reader.rs`), and this is what holds the match path there: five threads in the
/// store, two of them the search's.
#[tokio::test]
async fn select_all_in_a_saved_search_selects_only_its_matches() {
    let envelopes = vec![
        (0, dana("d1", "dt1", BASE_MS, "Label_1", false)),
        (0, dana("d2", "dt2", BASE_MS + 60_000, "Label_1", false)),
        (0, mail_envelope("w1", "wt1", BASE_MS + 120_000)),
        (0, mail_envelope("w2", "wt2", BASE_MS + 180_000)),
        (1, mail_envelope("h1", "ht1", BASE_MS + 240_000)),
    ];
    let mut f = saved_fixture(&envelopes, &[("dana", "from:dana")]).await;
    sidebar_to(&mut f, None, "dana").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(subjects(&f), ["subject d2", "subject d1"]);
    let listed = listed_ids(&f);
    assert_eq!(listed.len(), 2, "two of the five threads: {listed:?}");

    press(&mut f, KeyCode::Char('*')).await;
    press(&mut f, KeyCode::Char('a')).await;

    assert!(
        f.screen.header_left().contains("· 2 selected"),
        "the header counts the 2 selected threads"
    );
    assert_eq!(selected_ids(&f), listed, "exactly the rows it lists");
}

/// #110: a saved query with no condition (only words the full-text query drops) lists
/// nothing, counts nothing, and a select-all in it takes nothing — never every thread.
#[tokio::test]
async fn a_saved_search_with_no_condition_lists_and_selects_nothing() {
    let mut f = saved_fixture(&five_threads(), &[("everything", "* ( )")]).await;
    // No count is drawn: a count of 0 shows nothing, where the bug counted every unread one.
    assert_eq!(saved_rows(&f), [("everything".to_string(), None)]);
    sidebar_to(&mut f, None, "everything").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(subjects(&f).is_empty(), "{:?}", subjects(&f));
    press(&mut f, KeyCode::Char('*')).await;
    press(&mut f, KeyCode::Char('a')).await;
    assert!(selected_ids(&f).is_empty(), "{:?}", selected_ids(&f));
}

/// AC-19 (FR-19): `:search-counts` turns the counts off and on, saving, nothing else touched.
#[tokio::test]
async fn search_counts_toggles_and_saves() {
    let envelopes = vec![(0, dana("d1", "dt1", BASE_MS, "INBOX", false))];
    let mut f = saved_fixture(&envelopes, &[("dana", "from:dana")]).await;
    write_config(&f, "[general]\neditor = \"vi\" # mine\n");
    sidebar_to(&mut f, None, "dana").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(saved_rows(&f), [("dana".to_string(), Some(1))]);
    let row = drawn_saved_row(&f, "dana");
    assert!(row.ends_with('1'), "the count is drawn: {row:?}");
    assert!(
        f.screen.header_left().contains(" unread"),
        "the header shows the open search's unread count"
    );

    run_from_the_palette(&mut f, "search-counts").await;
    let text = config_text(&f);
    assert!(text.contains("[general]"), "{text}");
    assert!(text.contains("saved_search_counts = false"), "{text}");
    assert!(text.contains("editor = \"vi\" # mine"), "{text}");
    let row = drawn_saved_row(&f, "dana");
    assert!(
        !row.chars().any(|c| c.is_ascii_digit()),
        "no count is drawn: {row:?}"
    );
    assert!(
        !f.screen.header_left().contains(" unread"),
        "the header leaves the unread count out while counts are off"
    );

    run_from_the_palette(&mut f, "search-counts").await;
    let text = config_text(&f);
    assert!(
        !text.contains("saved_search_counts"),
        "true is the default: {text}"
    );
    let row = drawn_saved_row(&f, "dana");
    assert!(row.ends_with('1'), "the count is back: {row:?}");
    assert!(
        f.screen.header_left().contains(" unread"),
        "the header shows the unread count again"
    );
}

/// §6 slot 10 / §10: the four `save_search_prompt_*` snapshots AC-4 leans on.
#[tokio::test]
async fn the_save_prompt_draws_at_both_sizes_and_icon_sets() {
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        for (width, height) in [(90_u16, 24_u16), (150, 40)] {
            let mut f = saved_fixture(&five_threads(), &[]).await;
            f.screen.ctx.icons = icons;
            set_filter(&mut f, "from:dana").await;
            press(&mut f, KeyCode::Char('S')).await;
            type_text(&mut f, "needs-reply").await;
            insta::assert_snapshot!(
                format!("save_search_prompt_{name}_{width}x{height}"),
                shot(&f, width, height)
            );
        }
    }
}
