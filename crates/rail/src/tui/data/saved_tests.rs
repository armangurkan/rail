//! The two saved-search `MailEvent`s and what their `Debug` is allowed to print. Kept out of
//! `tests.rs`, which is already past the 800-line rule this branch must not push further.

use super::*;

/// AC-29 (§8 Privacy): a saved query holds addresses and subject fragments, so the two
/// saved-search events print counts and flags only — never a name, never a query, and never
/// the reason a config write failed.
#[test]
fn the_saved_search_events_log_names_and_numbers_only() {
    let changed = MailEvent::SavedSearchesChanged {
        result: Ok(vec![rail_core::config::SavedSearch {
            name: "needs-reply".into(),
            query: "from:dana@corp.example".into(),
        }]),
        renamed: None,
    };
    let text = format!("{changed:?}");
    assert!(text.contains("len: 1"), "{text}");
    assert!(!text.contains("needs-reply"), "{text}");
    assert!(!text.contains("dana"), "{text}");

    let failed = MailEvent::SavedSearchesChanged {
        result: Err(rail_core::ops::OpError::internal("gone")),
        renamed: None,
    };
    assert_eq!(
        format!("{failed:?}"),
        "SavedSearchesChanged { ok: false, len: 0, renamed: false }"
    );

    // The rename a `put`'s answer carries is two names, so only whether there is one prints.
    let renamed = MailEvent::SavedSearchesChanged {
        result: Ok(Vec::new()),
        renamed: Some(("needs-reply".into(), "later".into())),
    };
    let text = format!("{renamed:?}");
    assert!(text.contains("renamed: true"), "{text}");
    assert!(!text.contains("needs-reply"), "{text}");
    assert!(!text.contains("later"), "{text}");

    let saved = MailEvent::SearchCountsSaved {
        result: Err("disk full at /Users/me".into()),
    };
    let text = format!("{saved:?}");
    assert!(text.contains("ok: false"), "{text}");
    assert!(!text.contains("disk"), "{text}");
}
