//! The sidebar's tree model: the default folds, the `More folders` tree, folded sums, deep
//! paths, row scopes and keys, and where the cursor goes after a rebuild.

use super::sidebar_tree::fixtures::{accounts, folder, two_accounts};
use super::sidebar_tree::{Folds, RowKey, RowKind, SidebarRow, build, refind, visible};
use crate::tui::data::{ScopeChoice, SidebarOutcome};
use rail_core::provider::FolderKind;

/// Each row as `{indent}{fold} {label} ({count})`: two spaces per depth, `>` folded, `v`
/// unfolded, `-` not foldable.
fn shape(rows: &[SidebarRow]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let fold = match (r.foldable, r.folded) {
                (false, _) => "-",
                (true, true) => ">",
                (true, false) => "v",
            };
            let count = r.count.map_or(String::new(), |c| format!(" ({c})"));
            let indent = "  ".repeat(usize::from(r.depth));
            format!("{indent}{fold} {}{count}", r.label)
                .trim_end()
                .to_string()
        })
        .collect()
}

fn shown(outcome: &SidebarOutcome, folds: &Folds) -> Vec<String> {
    shape(&visible(&build(&accounts(), outcome, folds), folds))
}

fn unfolded(keys: &[RowKey]) -> Folds {
    let mut folds = Folds::default();
    for key in keys {
        folds.set(key, false);
    }
    folds
}

/// `n` rows of `rows` starting at the one equal to `first`.
fn from<'a>(rows: &'a [String], first: &str, n: usize) -> &'a [String] {
    let at = rows.iter().position(|r| r == first).expect(first);
    &rows[at..at + n]
}

/// Account `i`'s address, its rows' key.
fn acct(i: usize) -> String {
    accounts()[i].address.clone()
}

fn prefix(account: usize, path: &str) -> RowKey {
    RowKey::Prefix {
        account: acct(account),
        path: path.into(),
    }
}

#[test]
fn by_default_accounts_show_their_system_folders_and_fold_the_rest() {
    assert_eq!(
        shown(&two_accounts(), &Folds::default()),
        [
            "- UNIFIED",
            "- All inboxes (15)",
            "- Starred (12)",
            "- Drafts (3)",
            "- Sent",
            "- Trash",
            "-",
            "- ACCOUNTS",
            "v Personal (5)",
            "  - Inbox (5)",
            "  - Sent",
            "  - Drafts (2)",
            "  - Spam (0)",
            "  - Trash",
            "  -",
            "  > More folders (7)",
            "v AXON (10)",
            "  - Inbox (10)",
            "  - Sent Items",
            "  - Drafts (1)",
            "  - Archive (0)",
            "  - Junk Email (0)",
            "  - Deleted Items",
            "  -",
            "  > More folders (5)",
        ]
    );
}

#[test]
fn more_folders_holds_the_rest_as_a_sorted_tree_with_folded_parents_summed() {
    let rows = shown(
        &two_accounts(),
        &unfolded(&[RowKey::More(acct(0)), RowKey::More(acct(1))]),
    );
    assert_eq!(
        from(&rows, "  v More folders (7)", 5),
        [
            "  v More folders (7)",
            "    > [Imap] (0)",
            "    > Categories (7)",
            "    - Important (0)",
            "    > Shopping (3)",
        ],
        "parents and leaves together by name, ignoring case"
    );
    assert_eq!(
        from(&rows, "  v More folders (5)", 4),
        [
            "  v More folders (5)",
            "    - 1 - Saved (0)",
            "    > Inbox (7)",
            "    - Outbox (0)",
        ]
    );
}

/// Both `More folders` open with their top parents, `Infrastructure` (folder 38) left folded.
fn both_trees_open() -> Folds {
    unfolded(&[
        RowKey::More(acct(0)),
        prefix(0, "[Imap]"),
        prefix(0, "Categories"),
        RowKey::Folder {
            account: acct(0),
            folder_id: 16,
        },
        RowKey::More(acct(1)),
        prefix(1, "Inbox"),
    ])
}

#[test]
fn unfolded_parents_show_their_own_count_and_their_children() {
    let folds = both_trees_open();
    let rows = shown(&two_accounts(), &folds);
    assert_eq!(
        from(&rows, "  v More folders (7)", 10),
        [
            "  v More folders (7)",
            "    v [Imap]",
            "      - Sent (0)",
            "      - Trash (0)",
            "    v Categories",
            "      - Social (3)",
            "      - Updates (4)",
            "    - Important (0)",
            "    v Shopping (1)",
            "      - dhgate (2)",
        ]
    );
    assert_eq!(
        from(&rows, "  v More folders (5)", 5),
        [
            "  v More folders (5)",
            "    - 1 - Saved (0)",
            "    v Inbox",
            "      > Infrastructure (7)",
            "    - Outbox (0)",
        ],
        "Infrastructure is a folder with folders under it: folded, it sums them and itself"
    );
    let mut open = folds.clone();
    open.set(
        &RowKey::Folder {
            account: acct(1),
            folder_id: 38,
        },
        false,
    );
    let rows = shown(&two_accounts(), &open);
    assert_eq!(
        from(&rows, "      v Infrastructure (1)", 3),
        [
            "      v Infrastructure (1)",
            "        - Hetzner (2)",
            "        - Linode (4)",
        ]
    );
}

#[test]
fn a_path_deeper_than_six_segments_is_joined_at_the_sixth() {
    let mut outcome = two_accounts();
    outcome
        .counts
        .folders
        .push(folder(1, 50, "a/b/c/d/e/f/g", "user", 1));
    let mut keys = vec![RowKey::More(acct(0))];
    keys.extend(
        ["a", "a/b", "a/b/c", "a/b/c/d", "a/b/c/d/e"]
            .iter()
            .map(|p| prefix(0, p)),
    );
    let folds = unfolded(&keys);
    let rows = visible(&build(&accounts(), &outcome, &folds), &folds);
    let deep: Vec<(&str, u8)> = rows
        .iter()
        .skip_while(|r| r.label != "a")
        .take(6)
        .map(|r| (r.label.as_str(), r.depth))
        .collect();
    assert_eq!(
        deep,
        [("a", 2), ("b", 3), ("c", 4), ("d", 5), ("e", 6), ("f/g", 7)]
    );
}

#[test]
fn rows_open_their_scopes_and_keep_their_keys() {
    let folds = unfolded(&[RowKey::More(acct(0))]);
    let rows = visible(&build(&accounts(), &two_accounts(), &folds), &folds);
    let find = |label: &str| rows.iter().find(|r| r.label == label).expect(label);
    assert_eq!(find("Personal").scope(), Some(ScopeChoice::Account(0)));
    let inbox = rows
        .iter()
        .find(|r| r.label == "Inbox" && r.account() == Some(0))
        .unwrap();
    assert_eq!(
        inbox.scope(),
        Some(ScopeChoice::Account(0)),
        "the Inbox row opens the account's inbox"
    );
    let sent = rows
        .iter()
        .find(|r| r.label == "Sent" && r.account() == Some(0))
        .unwrap();
    assert!(
        matches!(
            sent.scope(),
            Some(ScopeChoice::Folder {
                kind: FolderKind::Sent,
                ..
            })
        ),
        "a folder row's scope carries its kind"
    );
    assert_eq!(find("Starred").scope(), Some(ScopeChoice::Starred));
    assert_eq!(find("Starred").key, RowKey::Unified("Starred"));
    assert_eq!(find("More folders (7)").scope(), None);
    assert_eq!(find("[Imap]").scope(), None, "a prefix opens nothing");
    assert_eq!(find("[Imap]").key, prefix(0, "[Imap]"));
    let shopping = find("Shopping");
    assert!(matches!(
        &shopping.kind,
        RowKind::Parent {
            scope: Some(ScopeChoice::Folder { folder_id: 16, .. }),
            ..
        }
    ));
    assert_eq!(
        shopping.key,
        RowKey::Folder {
            account: acct(0),
            folder_id: 16
        }
    );
    assert_eq!(
        shopping.parents,
        [RowKey::Account(acct(0)), RowKey::More(acct(0))],
        "what must be unfolded for it to show"
    );
    assert!(!find("ACCOUNTS").selectable() && find("Personal").selectable());
    assert!(
        !rows
            .iter()
            .any(|r| r.kind == RowKind::Rule && r.selectable())
    );
}

#[test]
fn a_folded_account_hides_everything_under_it() {
    let mut folds = Folds::default();
    assert!(!folds.is_folded(&RowKey::Account(acct(1))) && folds.is_folded(&RowKey::More(acct(1))));
    folds.set(&RowKey::Account(acct(1)), true);
    let rows = shown(&two_accounts(), &folds);
    assert_eq!(rows.last().map(String::as_str), Some("> AXON (10)"));
    folds.set(&RowKey::Account(acct(1)), false);
    assert!(!folds.is_folded(&RowKey::Account(acct(1))));
}

#[test]
fn an_account_with_only_system_folders_has_no_rule_and_no_more_folders() {
    let mut outcome = two_accounts();
    outcome
        .counts
        .folders
        .retain(|f| f.account_id == 1 || f.folder_id <= 36);
    let rows = shown(&outcome, &Folds::default());
    assert_eq!(rows.last().map(String::as_str), Some("  - Deleted Items"));
    outcome.account_ids = vec![Some(1), None];
    let rows = shown(&outcome, &Folds::default());
    assert_eq!(
        rows.last().map(String::as_str),
        Some("v AXON (0)"),
        "an account not synced yet has its row and nothing under it"
    );
}

/// Review Focus 3.
#[test]
fn the_cursor_stays_on_its_row_or_moves_to_the_nearest_one_above() {
    let shopping = RowKey::Folder {
        account: acct(0),
        folder_id: 16,
    };
    let dhgate = RowKey::Folder {
        account: acct(0),
        folder_id: 17,
    };
    let folds = unfolded(&[RowKey::More(acct(0)), shopping.clone()]);
    let old = visible(&build(&accounts(), &two_accounts(), &folds), &folds);
    assert_eq!(refind(&old, Some(&dhgate), &old), Some(dhgate.clone()));
    let mut outcome = two_accounts();
    outcome.counts.folders.retain(|f| f.folder_id != 17);
    let new = visible(&build(&accounts(), &outcome, &folds), &folds);
    assert_eq!(
        refind(&old, Some(&dhgate), &new),
        Some(shopping),
        "gone: Shopping, just above it"
    );
    let folded = Folds::default();
    let hidden = visible(&build(&accounts(), &two_accounts(), &folded), &folded);
    assert_eq!(
        refind(&old, Some(&dhgate), &hidden),
        Some(RowKey::More(acct(0))),
        "folded away: the nearest shown row above"
    );
    assert_eq!(
        refind(&[], None, &hidden),
        Some(RowKey::Unified("All inboxes")),
        "no cursor yet: the first row it can stop on"
    );
}
