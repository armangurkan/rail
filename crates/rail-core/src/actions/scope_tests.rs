//! The scope rule's table: every `Change` against a thread with a message in each of Inbox,
//! Sent, Drafts, Deleted Items, a user folder and Archive, in mixed read and flag states; the
//! newest message and its tie-break; a message with no folder; an empty thread.

use super::scope::touched;
use crate::provider::{Change, FolderKind, TargetMessage};

fn m(
    id: &str,
    folder: Option<(FolderKind, &str)>,
    unread: bool,
    starred: bool,
    date: i64,
) -> TargetMessage {
    TargetMessage {
        id: id.into(),
        folder: folder.map(|(kind, _)| kind),
        folder_remote: folder.map(|(_, remote)| remote.to_string()),
        unread,
        starred,
        date,
    }
}

/// 0 Inbox unread, 1 Sent, 2 Drafts (the newest of all), 3 Deleted Items unread, 4 Projects
/// flagged (the newest received), 5 Archive.
fn spread() -> Vec<TargetMessage> {
    vec![
        m("a", Some((FolderKind::Inbox, "f-inbox")), true, false, 10),
        m("b", Some((FolderKind::Sent, "f-sent")), false, false, 20),
        m(
            "c",
            Some((FolderKind::Drafts, "f-drafts")),
            false,
            false,
            30,
        ),
        m("d", Some((FolderKind::Trash, "f-del")), true, false, 5),
        m("e", Some((FolderKind::User, "f-proj")), false, true, 15),
        m("f", Some((FolderKind::Archive, "f-arch")), false, false, 1),
    ]
}

#[test]
fn each_change_touches_what_the_spec_table_says() {
    let to = |id: &str| Change::Move { to: id.into() };
    let cases: [(Change, Vec<usize>); 10] = [
        (Change::Archive, vec![0]),
        (Change::Trash, vec![0, 4, 5]),
        (to("f-proj"), vec![0, 3, 5]),
        (to("f-inbox"), vec![3, 4, 5]),
        (Change::Read { on: true }, vec![0, 3]),
        (Change::Read { on: false }, vec![4]),
        (Change::Star { on: true }, vec![]),
        (Change::Star { on: false }, vec![4]),
        (
            Change::Labels {
                add: vec!["f-proj".into()],
                remove: vec![],
            },
            vec![],
        ),
        (to("f-arch"), vec![0, 3, 4]),
    ];
    let thread = spread();
    for (change, expected) in cases {
        assert_eq!(touched(&change, &thread), expected, "{change:?}");
    }
}

#[test]
fn star_on_flags_the_newest_only_when_nothing_is_flagged() {
    let mut thread = spread();
    thread[4].starred = false;
    assert_eq!(touched(&Change::Star { on: true }, &thread), [4]);
}

#[test]
fn unread_marks_the_newest_only_when_it_is_read() {
    let mut thread = spread();
    thread[4].unread = true;
    assert!(touched(&Change::Read { on: false }, &thread).is_empty());
}

#[test]
fn newest_skips_the_users_own_mail_unless_the_thread_has_nothing_else() {
    let thread = [
        m("s1", Some((FolderKind::Sent, "f-sent")), false, false, 5),
        m("s2", Some((FolderKind::Sent, "f-sent")), false, false, 9),
    ];
    assert_eq!(touched(&Change::Read { on: false }, &thread), [1]);
    assert_eq!(touched(&Change::Star { on: true }, &thread), [1]);
}

#[test]
fn a_tie_on_date_goes_to_the_larger_id() {
    let inbox = Some((FolderKind::Inbox, "f-inbox"));
    let thread = [
        m("m-b", inbox, false, false, 7),
        m("m-a", inbox, false, false, 7),
    ];
    assert_eq!(touched(&Change::Read { on: false }, &thread), [0]);
    assert_eq!(touched(&Change::Star { on: true }, &thread), [0]);
}

#[test]
fn a_message_with_no_folder_is_trashed_and_moved_but_not_archived() {
    let thread = [m("x", None, true, false, 1)];
    assert!(touched(&Change::Archive, &thread).is_empty());
    assert_eq!(touched(&Change::Trash, &thread), [0]);
    assert_eq!(
        touched(
            &Change::Move {
                to: "f-proj".into()
            },
            &thread
        ),
        [0]
    );
}

#[test]
fn an_empty_thread_touches_nothing() {
    for change in [
        Change::Archive,
        Change::Trash,
        Change::Star { on: true },
        Change::Star { on: false },
        Change::Read { on: true },
        Change::Read { on: false },
        Change::Move { to: "f".into() },
    ] {
        assert!(touched(&change, &[]).is_empty(), "{change:?}");
    }
}
