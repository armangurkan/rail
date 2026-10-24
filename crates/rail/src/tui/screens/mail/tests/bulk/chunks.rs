//! A bulk selection sent and taken back: `dispatch` chunked by `MAX_ACTION_THREADS`, one
//! client token per chunk, `z` chunked by `MAX_UNDO_ROWS` reporting once, a mixed undo answer,
//! and a failed chunk putting back only its own threads. A child module of `tests/bulk.rs`,
//! whose helpers it uses, because the two together would cross CONTRIBUTING's 800-line cap.

use super::*;

#[tokio::test]
async fn a_big_selection_is_sent_in_chunks_of_two_hundred() {
    let mut f = fixture(450).await;
    let calls = recording(&f, None);
    // In the unified scope `* a` would take home's two as well.
    press(&mut f, KeyCode::Char('1')).await;
    chord(&mut f, '*', 'a').await;
    assert_eq!(f.screen.actions.selection.len(), 450);
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('y')).await;
    let chunks: Vec<Vec<i64>> = applies(&f)
        .iter()
        .map(|call| match call {
            FakeCall::Apply {
                account,
                threads,
                action,
                grace,
            } => {
                assert_eq!(account, "work");
                assert_eq!(*action, MailAction::Archive {});
                assert!(*grace);
                threads.clone()
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [200, 200, 50]
    );
    let sent = chunks.concat();
    let mut ascending = sent.clone();
    ascending.sort_unstable();
    ascending.dedup();
    assert_eq!(sent, ascending, "ascending by thread id, no id repeated");
    let tokens: BTreeSet<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.client_token.clone())
        .collect();
    assert_eq!(tokens.len(), 3, "one client token per chunk, all distinct");
}

#[tokio::test]
async fn chunks_never_mix_accounts() {
    let mut f = fixture(2).await;
    let _calls = recording(&f, None);
    chord(&mut f, '*', 'a').await;
    assert_eq!(f.screen.actions.selection.len(), 4);
    press(&mut f, KeyCode::Char('e')).await;
    let mut accounts: Vec<String> = Vec::new();
    for call in applies(&f) {
        let FakeCall::Apply {
            account, threads, ..
        } = call
        else {
            panic!("not an apply")
        };
        for thread in &threads {
            assert_eq!(
                account_of(&f, *thread),
                account,
                "no call mixes accounts: {threads:?}"
            );
        }
        accounts.push(account);
    }
    accounts.sort();
    accounts.dedup();
    assert_eq!(accounts, ["home", "work"], "at least one call per account");
}

#[tokio::test]
async fn one_z_undoes_every_chunks_row_in_one_call() {
    let mut f = fixture(450).await;
    let calls = recording(&f, None);
    press(&mut f, KeyCode::Char('1')).await;
    chord(&mut f, '*', 'a').await;
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('y')).await;
    press(&mut f, KeyCode::Char('z')).await;
    let taken = undos(&f);
    let [FakeCall::Undo { rows }] = &taken[..] else {
        panic!("{taken:?}")
    };
    assert_eq!(rows.len(), 3, "one outbox row per `action.apply` call");
    let tokens: BTreeSet<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.client_token.clone())
        .collect();
    assert!(
        rows.iter().all(|row| tokens.contains(&row.client_token)),
        "{rows:?} against {tokens:?}"
    );
    let line = f.screen.status_line();
    assert_eq!(line.matches("undone").count(), 1, "{line}");
}

#[tokio::test]
async fn the_undo_split_holds_at_sixty_four_rows() {
    let mut f = fixture(3).await;
    *f.fake.on_apply.lock().unwrap() = Some(Box::new(|call: &ApplyCall| {
        Ok(ActionApplyResponse {
            rows: (1..=100).collect(),
            applied: call.threads.len(),
        })
    }));
    // The cursor row alone: under the threshold, so nothing is asked.
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('z')).await;
    let taken = undos(&f);
    let sizes: Vec<usize> = taken
        .iter()
        .map(|call| match call {
            FakeCall::Undo { rows } => rows.len(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(sizes, [64, 36], "two calls for a hundred rows");
    let mut undone: Vec<i64> = taken
        .iter()
        .flat_map(|call| match call {
            FakeCall::Undo { rows } => rows.iter().map(|row| row.row).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        })
        .collect();
    undone.sort_unstable();
    assert_eq!(undone, (1..=100).collect::<Vec<i64>>(), "each row once");
    let line = f.screen.status_line();
    assert_eq!(line.matches("undone").count(), 1, "{line}");
}

#[tokio::test]
async fn a_mixed_undo_answer_says_undone_in_part() {
    let mut f = fixture(450).await;
    let _calls = recording(&f, None);
    press(&mut f, KeyCode::Char('1')).await;
    chord(&mut f, '*', 'a').await;
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('y')).await;
    *f.fake.undo_answer.lock().unwrap() = Some(Ok(ActionUndoResponse {
        undone: vec![1, 2],
        too_late: vec![3],
    }));
    press(&mut f, KeyCode::Char('z')).await;
    let line = f.screen.status_line();
    assert!(line.contains("undone in part"), "{line}");
    assert!(!line.contains("too late to undo"), "{line}");
}

#[tokio::test]
async fn a_failed_chunk_restores_only_its_own_threads() {
    let mut f = fixture(450).await;
    press(&mut f, KeyCode::Char('1')).await;
    let work = ids_of(&f, 1);
    let ids = work[..300].to_vec();
    let second = ids[200..].to_vec();
    let _calls = recording(&f, Some(second.clone()));
    select(&mut f, 1, &ids);
    press(&mut f, KeyCode::Char('e')).await;
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(selected_ids(&f), second, "only the failed chunk comes back");
}
