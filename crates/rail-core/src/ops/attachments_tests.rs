//! The two attachment op names and their place outside the retry-safe set, and the count-only
//! `Debug` every attachment type on the wire has.

use super::attachments::*;
use super::{OpError, names};

/// A populated save request.
fn save_request() -> AttachmentSaveRequest {
    AttachmentSaveRequest {
        account: "work".into(),
        message_id: "m1".into(),
        part_ids: vec!["0.1".into()],
        dir: "/x".into(),
    }
}

/// A populated save response: one part written into `/x`.
fn save_response() -> AttachmentSaveResponse {
    AttachmentSaveResponse {
        results: vec![AttachmentResult {
            part_id: "0.1".into(),
            outcome: AttachmentOutcome::Saved {
                path: "/x/plan.pdf".into(),
                size: 1,
            },
        }],
    }
}

/// AC-29a: both names are in `ALL` at the pinned slots, and neither may be resent.
#[test]
fn the_two_names_exist_and_are_not_retry_safe() {
    assert_eq!(names::ATTACHMENT_SAVE, "attachment.save");
    assert_eq!(names::ATTACHMENT_OPEN, "attachment.open");
    // Slots 25 and 26: appended after `draft.import` and adjacent in that order.
    assert_eq!(names::ALL[24], names::ATTACHMENT_SAVE);
    assert_eq!(names::ALL[25], names::ATTACHMENT_OPEN);
    let after_import = names::ALL
        .iter()
        .position(|name| *name == names::DRAFT_IMPORT)
        .expect("draft.import is in ALL");
    assert_eq!(after_import + 1, 24, "the pair follows draft.import");
    assert!(!crate::daemon::retry_safe(names::ATTACHMENT_SAVE));
    assert!(!crate::daemon::retry_safe(names::ATTACHMENT_OPEN));
}

/// AC-32 (the `ops` half): no request, response, result or outcome prints a filename, a part
/// id, a directory, a path, a mime type or an account — only counts, sizes and variant names.
#[test]
fn debug_prints_counts_only() {
    let printed = [
        format!("{:?}", save_request()),
        format!("{:?}", save_response()),
        format!(
            "{:?}",
            AttachmentOpenRequest {
                account: "work".into(),
                message_id: "m1".into(),
                part_id: "0.1".into(),
            }
        ),
        format!(
            "{:?}",
            AttachmentOpenResponse {
                path: "/x/plan.pdf".into(),
            }
        ),
        format!("{:?}", save_response().results[0]),
        format!("{:?}", save_response().results[0].outcome),
        format!(
            "{:?}",
            AttachmentResult {
                part_id: "0.2".into(),
                outcome: AttachmentOutcome::Missing,
            }
        ),
        format!(
            "{:?}",
            AttachmentResult {
                part_id: "0.3".into(),
                outcome: AttachmentOutcome::Failed(OpError::invalid_input(
                    "dir: not an absolute path"
                )),
            }
        ),
    ];
    for text in &printed {
        for secret in [
            "plan",
            "/x",
            "work",
            "m1",
            "0.1",
            "0.2",
            "0.3",
            "application/pdf",
            "absolute",
        ] {
            assert!(!text.contains(secret), "{text} carries {secret}");
        }
    }
    assert!(printed[1].contains('1'), "the result count: {}", printed[1]);
    assert!(
        printed[5].contains("Saved") && printed[5].contains('1'),
        "the variant and the size: {}",
        printed[5]
    );
    assert!(printed[6].contains("Missing"), "{}", printed[6]);
    assert!(printed[7].contains("Failed"), "{}", printed[7]);
}
