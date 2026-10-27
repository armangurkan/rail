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

/// A fixture whose palette is open over `THREE` while `MailScreen.saved` has already become
/// `[b-list, c-list]`. The palette is built once, when it opens, so every entry in it now
/// carries a position the list no longer backs: `c-list`'s is past the end, and `a-list`'s
/// holds `b-list`.
async fn palette_open_over_a_changed_list() -> Fixture {
    let mut f = saved_fixture(&one_each(), &THREE).await;
    press(&mut f, KeyCode::Char(':')).await;
    answer_without_the_sidebar_read(&mut f, &THREE[1..]);
    f
}

/// Type `text` into the palette that is already open, check the line `Enter` will run is
/// `expect` — the match order is the fuzzy matcher's, not this test's business — and run it.
async fn choose_from_the_open_palette(f: &mut Fixture, text: &str, expect: &str) {
    type_text(f, text).await;
    let top = match &f.screen.modal {
        Some(Modal::Palette(palette)) => palette
            .shown()
            .first()
            .map_or_else(String::new, |entry| entry.text.clone()),
        _ => panic!("no palette is open for {text:?}"),
    };
    assert_eq!(top, expect, "the line Enter runs");
    press(f, KeyCode::Enter).await;
}

/// G2 (FR-16, Q-5): the palette's `Rename search:` and `Remove search:` lines act on the search
/// their own text names. They are built once with the list, so an answer landing while the
/// palette is up leaves each one a position: the line saying `c-list` would reach whatever sits
/// at that index — past the end, nothing at all — and the line saying a name that has gone
/// would reach its neighbour and remove or rename a search the user never pointed at.
#[tokio::test]
async fn the_palette_s_rename_and_remove_act_on_the_search_they_name() {
    let mut f = palette_open_over_a_changed_list().await;
    choose_from_the_open_palette(&mut f, "remove-search c-list", "Remove search: c-list").await;
    assert_eq!(
        confirm_lines(&f),
        ["Remove the saved search \"c-list\"? The mail it finds is not touched."]
    );
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchRemove {
            name: "c-list".into()
        }],
        "the search the line named, and only it"
    );

    // The rename's prompt opens on the same search: `Enter` on it unedited names what it
    // opened on, so the put that leaves says which search the prompt was about.
    let mut f = palette_open_over_a_changed_list().await;
    choose_from_the_open_palette(&mut f, "rename-search c-list", "Rename search: c-list").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchPut {
            name: "c-list".into(),
            query: "subject:c1".into(),
            replaces: Some("c-list".into()),
        }],
        "the prompt opened on the search the line named"
    );

    // `a-list` is gone (index 0 holds `b-list`): both say the searches moved, like a stale r/d.
    for (text, top) in [
        ("remove-search a-list", "Remove search: a-list"),
        ("rename-search a-list", "Rename search: a-list"),
    ] {
        let mut f = palette_open_over_a_changed_list().await;
        choose_from_the_open_palette(&mut f, text, top).await;
        assert!(f.screen.modal.is_none(), "{text} offered something");
        assert!(
            f.screen
                .status_line()
                .contains("The saved searches changed; try again."),
            "{text}: {}",
            f.screen.status_line()
        );
        assert_eq!(saved_calls(&f), Vec::new(), "{text} sent something");
    }
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
        "the header falls back to All inboxes"
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
        "the header titles the search the row names"
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
