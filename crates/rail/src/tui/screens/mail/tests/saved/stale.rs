//! A sidebar row, or a palette entry, built from a list a write has already replaced: the
//! answer lands and `MailScreen.saved` changes, but the rows stay drawn until the sidebar read
//! the answer asked for comes back. A saved scope is a position, so in that window a position
//! names the wrong search, or none. Nothing may resolve by position alone.

use super::*;

/// The `[[saved_searches]]` list an answer carries, `(name, query)` in config order.
fn list(searches: &[(&str, &str)]) -> Vec<SavedSearch> {
    searches
        .iter()
        .map(|(name, query)| SavedSearch {
            name: (*name).into(),
            query: (*query).into(),
        })
        .collect()
}

/// Three searches, each finding one thread of its own, so a mix-up names itself.
const THREE: [(&str, &str); 3] = [
    ("a-list", "subject:a1"),
    ("b-list", "subject:b1"),
    ("c-list", "subject:c1"),
];

/// One thread per search, all in INBOX, so All inboxes shows them and a wrong search shows
/// exactly one wrong subject.
fn one_each() -> Vec<(usize, Envelope)> {
    vec![
        (0, mail_envelope("a1", "at1", BASE_MS)),
        (0, mail_envelope("b1", "bt1", BASE_MS + 60_000)),
        (0, mail_envelope("c1", "ct1", BASE_MS + 120_000)),
    ]
}

/// The answer to a write that is not this test's, landing while the sidebar read it asks for
/// is still in flight: `MailScreen.saved` becomes `searches`, the rows do not. The effects are
/// dropped on purpose — that dropped `read_sidebar` **is** the window being tested.
fn answer_without_the_sidebar_read(f: &mut Fixture, searches: &[(&str, &str)]) {
    let effects = f.screen.saved_searches_changed(Ok(list(searches)), None);
    assert!(!effects.is_empty(), "the answer asked for a sidebar read");
    drop(effects);
}

/// G2 (FR-10, FR-12): `saved_filter` returns `None` for a position its list no longer holds,
/// and `store_scope` turns a `Saved` scope into `Scope::All` — every folder of every account.
/// Unnarrowed, that reads the whole mailbox under the row's title, and `* a` takes it. The two
/// threads here are outside every inbox, so the only way the list can hold them is that
/// unfiltered read; the fallback leaves All inboxes, which here is empty.
#[tokio::test]
async fn a_vanished_saved_search_never_opens_every_thread() {
    let archived = vec![
        (0, dana("d1", "dt1", BASE_MS, "Label_1", false)),
        (0, dana("d2", "dt2", BASE_MS + 60_000, "Label_1", false)),
    ];
    let searches = [("a-list", "from:dana"), ("b-list", "subject:dt2")];
    let mut f = saved_fixture(&archived, &searches).await;
    answer_without_the_sidebar_read(&mut f, &searches[..1]);

    sidebar_to(&mut f, None, "b-list").await;
    press(&mut f, KeyCode::Enter).await;

    assert!(
        subjects(&f).is_empty(),
        "every thread was read: {:?}",
        subjects(&f)
    );
    press(&mut f, KeyCode::Char('*')).await;
    press(&mut f, KeyCode::Char('a')).await;
    assert!(
        selected_ids(&f).is_empty(),
        "select-all took them: {:?}",
        selected_ids(&f)
    );
    // It fails the way a removal does: a named mailbox, not that search's title over
    // everything. The notice itself needs a mailbox with mail in it — the half below.
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
    assert!(
        f.screen.header_left().starts_with("▍Rail │ All inboxes"),
        "{}",
        f.screen.header_left()
    );

    // The same stale `Enter` where All inboxes has mail, so the notice is not replaced by the
    // empty list's own `Nothing matches.`
    let mut f = saved_fixture(&five_threads(), &searches).await;
    answer_without_the_sidebar_read(&mut f, &searches[..1]);
    sidebar_to(&mut f, None, "b-list").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
    assert!(
        f.screen
            .status_line()
            .contains("The saved search you had open is gone; showing All inboxes."),
        "{}",
        f.screen.status_line()
    );
}

/// G2: with `a-list` gone, `b-list` has moved to index 0 and the row still drawn at index 1
/// would open `c-list`'s threads under `b-list`'s title. The row the user points at is the
/// search the user means, so the scope is re-keyed by name.
#[tokio::test]
async fn a_stale_row_opens_its_own_search_by_name() {
    let mut f = saved_fixture(&one_each(), &THREE).await;
    answer_without_the_sidebar_read(&mut f, &THREE[1..]);

    sidebar_to(&mut f, None, "b-list").await;
    press(&mut f, KeyCode::Enter).await;

    assert_eq!(subjects(&f), ["subject b1"]);
    assert!(
        f.screen.header_left().starts_with("▍Rail │ Search: b-list"),
        "{}",
        f.screen.header_left()
    );
    // Matched, not compared: `ScopeChoice`'s `PartialEq` reads a `Saved`'s `index` only.
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Saved { index: 0, name } if name == "b-list"),
        "{:?}",
        f.screen.scope
    );
    assert_eq!(f.screen.status.notice(), None, "the row opened, quietly");
}

/// G2 (FR-16): `d` offers the search the cursor's row names, or says the searches changed —
/// never a search at that position that the row was never drawn for. The confirmation carries
/// the index, so without the check `y` would remove `c-list` while the row says `b-list`.
#[tokio::test]
async fn d_on_a_stale_row_asks_about_that_row_or_says_they_moved() {
    let mut f = saved_fixture(&one_each(), &THREE).await;
    sidebar_to(&mut f, None, "b-list").await;
    press(&mut f, KeyCode::Char('d')).await;
    assert_eq!(
        confirm_lines(&f),
        ["Remove the saved search \"b-list\"? The mail it finds is not touched."]
    );

    let mut f = saved_fixture(&one_each(), &THREE).await;
    answer_without_the_sidebar_read(&mut f, &THREE[1..]);
    sidebar_to(&mut f, None, "b-list").await;
    press(&mut f, KeyCode::Char('d')).await;
    assert!(f.screen.modal.is_none(), "it offered something");
    assert!(
        f.screen
            .status_line()
            .contains("The saved searches changed; try again."),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(saved_calls(&f), Vec::new(), "nothing was sent");
}
