//! How a woken snooze and a failed return settle: Rail taking the thread out of the inbox again,
//! or a sync finding it out of the inbox, ends the woken row, except while its return failed and
//! waits for a retry from `:errors` (#106).

use std::collections::BTreeSet;

use rusqlite::{Transaction, params};

use super::super::StoreError;
use super::super::outbox::json;
use crate::ops::Action;

/// Outbox row `$o` is a snooze's return still owed: its thread has no mail in the inbox, and
/// some message it names is still where the snooze's archive left it (`archived_by_labels!`,
/// `archived_in_folder!`), not trashed, reported or filed since. While it is failed, a sync
/// neither clears it nor settles its woken snooze (#106).
macro_rules! return_still_owed {
    ($o:literal) => {
        concat!(
            "json_extract(",
            $o,
            ".payload, '$.kind') = 'unsnooze'
             AND NOT EXISTS (SELECT 1 FROM json_each(",
            $o,
            ".thread_ids) rt
               JOIN messages m ON m.thread_id = rt.value
               JOIN message_folders mf ON mf.message_id = m.id
               JOIN folders f ON f.id = mf.folder_id
               WHERE f.kind = 'inbox')
             AND EXISTS (SELECT 1 FROM json_each(",
            $o,
            ".thread_ids) rt
               JOIN messages m ON m.thread_id = rt.value
               WHERE m.remote_id IN (SELECT value FROM json_each(",
            $o,
            ".payload, '$.messages'))
                 AND CASE (SELECT a.kind FROM accounts a WHERE a.id = ",
            $o,
            ".account_id) WHEN 'm365' THEN ",
            $crate::store::snoozes::archived_in_folder!(),
            " ELSE ",
            $crate::store::snoozes::archived_by_labels!(),
            " END)"
        )
    };
}
pub(in crate::store) use return_still_owed;

/// Rail archived, trashed or moved `threads` (or snoozes one again): a thread that came back from
/// a snooze leaves its woken row, so its rollup dates it by its mail again, and a return of it
/// that failed leaves `:errors` (#106).
pub(in crate::store) fn left_inbox(
    tx: &Transaction,
    account: i64,
    action: &Action,
    threads: &[i64],
) -> Result<(), StoreError> {
    if !matches!(
        action,
        Action::Archive {} | Action::Trash {} | Action::Move { .. }
    ) {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM snoozes WHERE account_id = ?1 AND woken_at IS NOT NULL
           AND EXISTS (SELECT 1 FROM threads t WHERE t.account_id = snoozes.account_id
                       AND t.remote_id = snoozes.thread_remote
                       AND t.id IN (SELECT value FROM json_each(?2)))",
        params![account, json(&threads)],
    )?;
    // A return that failed is not owed any more: the thread goes where Rail now sends it.
    tx.execute(
        "DELETE FROM outbox WHERE account_id = ?1 AND state = 'failed'
           AND json_extract(payload, '$.kind') = 'unsnooze'
           AND EXISTS (SELECT 1 FROM json_each(outbox.thread_ids) t
                       WHERE t.value IN (SELECT value FROM json_each(?2)))",
        params![account, json(&threads)],
    )?;
    Ok(())
}

/// A sync batch touched `threads`: one that came back from a snooze but is out of the inbox now
/// (archived elsewhere, or its return reverted) leaves its woken row, so the rollup dates it by
/// its mail again. Run after the waiting actions are applied again, so a return still on its way
/// keeps the thread in the inbox. A return that failed keeps the row too (`clear_failed` keeps
/// it for a retry), so a retry brings the thread back as the wake did.
pub(in crate::store) fn settle_woken(
    tx: &Transaction,
    account: i64,
    threads: &BTreeSet<i64>,
) -> Result<(), StoreError> {
    if threads.is_empty() {
        return Ok(());
    }
    tx.execute(
        concat!(
            "DELETE FROM snoozes WHERE account_id = ?1 AND woken_at IS NOT NULL
           AND EXISTS (SELECT 1 FROM threads t WHERE t.account_id = snoozes.account_id
                       AND t.remote_id = snoozes.thread_remote
                       AND t.id IN (SELECT value FROM json_each(?2))
                       AND NOT EXISTS (SELECT 1 FROM messages m
                                       JOIN message_folders mf ON mf.message_id = m.id
                                       JOIN folders f ON f.id = mf.folder_id
                                       WHERE m.thread_id = t.id AND f.kind = 'inbox')
                       AND NOT EXISTS (SELECT 1 FROM outbox o
                                       WHERE o.account_id = snoozes.account_id
                                         AND o.state = 'failed'
                                         AND EXISTS (SELECT 1 FROM json_each(o.thread_ids) ot
                                                     WHERE ot.value = t.id)
                                         AND ",
            return_still_owed!("o"),
            "))"
        ),
        params![account, json(threads)],
    )?;
    Ok(())
}
