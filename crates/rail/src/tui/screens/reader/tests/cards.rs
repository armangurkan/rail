//! The title block and the cards through the mail screen: `h` and `?`.

use crossterm::event::KeyCode;

use super::{all_text, draw, fixture, keys, open_toms_card, press, store_bodies_on_fetch};

#[tokio::test]
async fn h_shows_and_hides_the_headers_and_help_lists_it() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    assert!(!all_text(&f).contains("From:"), "hidden at first");
    keys(&mut f, "h").await;
    let text = all_text(&f);
    assert!(text.contains("From: Tom <tom@example.com>"), "{text}");
    assert!(text.contains("To: Dana Reyes <dana@example.com>"), "{text}");
    assert!(text.contains("Date: Thu 09 Oct 2025 08:53"), "{text}");
    assert!(
        text.contains("From: Sender <sender@example.com>"),
        "the newest's too: {text}"
    );
    assert!(
        f.screen
            .reader_cursor_text()
            .unwrap()
            .starts_with("▾   1   Tom"),
        "the cursor stays on Tom's card"
    );
    keys(&mut f, "h").await;
    assert!(!all_text(&f).contains("From:"), "{}", all_text(&f));
    keys(&mut f, "?").await;
    let help = draw(&f, 150, 40);
    assert!(help.contains("Show or hide the headers"), "{help}");
}
