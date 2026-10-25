//! `saved_search.put` and `saved_search.remove`: the wire names, and the two handlers through
//! a real daemon on a temporary `config.toml`. AC-20, AC-20b, AC-20c, AC-21, AC-28.

use crate::ops::names;

/// AC-28: asserted **relatively**, so it passes on this branch (where `ALL` is `[&str; 26]`
/// and the attachments consts do not exist) and still passes once that branch merges ahead of
/// it with its own pair. The retry clause asserts the public `retry_safe`, not `RETRY_SAFE`,
/// which is a private const of `connect.rs`; the array itself is covered by
/// `connect_tests.rs`'s `unsafe_op` list.
#[test]
fn the_saved_search_names_are_registered_and_never_retried() {
    let at = |name: &str| {
        names::ALL
            .iter()
            .position(|n| *n == name)
            .unwrap_or_else(|| panic!("{name} is not in names::ALL"))
    };
    let put = at(names::SAVED_SEARCH_PUT);
    let remove = at(names::SAVED_SEARCH_REMOVE);
    assert_eq!(put + 1, remove, "the two names are adjacent, put first");
    assert!(put > at(names::DRAFT_IMPORT), "appended after draft.import");

    // Neither carries a client token, so a resent `put` would be refused for a clashing name
    // and a resent `remove` would fail `NotFound`.
    assert!(!super::connect::retry_safe(names::SAVED_SEARCH_PUT));
    assert!(!super::connect::retry_safe(names::SAVED_SEARCH_REMOVE));

    let served = super::handlers::registry().names();
    assert!(served.contains(&names::SAVED_SEARCH_PUT), "{served:?}");
    assert!(served.contains(&names::SAVED_SEARCH_REMOVE), "{served:?}");
}
