//! `* f` on a thread the user answered last selects by the correspondent, not by the user's own
//! address. A child module of `tests/bulk.rs`, whose helpers it uses.

use super::*;

#[tokio::test]
async fn select_sender_passes_over_the_users_own_reply() {
    let mut f = fixture(3).await;
    let mut dana = from_sender("r0", BASE_MS + 29_000_000, None, "dana@example.com");
    dana.thread_id = "rt".into();
    // The user's reply, newest in the thread, from `work`'s own address.
    let mut reply = from_sender("r1", BASE_MS + 30_000_000, None, "w@example.com");
    reply.thread_id = "rt".into();
    apply_batch(&f, vec![dana, reply]);
    store_changed(&mut f).await;
    press(&mut f, KeyCode::Home).await;
    assert_eq!(
        f.screen.list.selected().unwrap().key.thread_id,
        thread_of(&f, "rt"),
        "the cursor is on the answered thread"
    );
    chord(&mut f, '*', 'f').await;
    assert_eq!(f.screen.slot_text(), "/ from:<dana@example.com>");
    assert_eq!(selected_ids(&f), vec![thread_of(&f, "rt")]);
}
