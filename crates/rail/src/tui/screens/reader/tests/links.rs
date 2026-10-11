//! `g x`'s confirmation (the host, the warning about text before `@`, a mail recipient) and
//! plain link markers while a modal is open.

use crossterm::event::KeyCode;
use rail_core::store::Store;

use super::{Fixture, cursor_to, draw, fixture, keys, press, store_bodies_on_fetch};
use crate::tui::screen::Screen;
use crate::tui::theme::Theme;

/// Store `links` as m1's link list; the fixture's body has markers `[1]` and `[2]` on one line.
fn set_links(f: &Fixture, links: &[&str]) {
    let json = serde_json::to_string(links).unwrap();
    Store::open(&f.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "UPDATE messages SET links = ?1 WHERE remote_id = 'm1'",
            [json],
        )
        .unwrap();
}

/// Open the thread, expand m1 and put the cursor on its line with both markers.
async fn open_on_the_link_line(f: &mut Fixture) {
    press(f, KeyCode::Enter).await;
    press(f, KeyCode::Home).await;
    press(f, KeyCode::Tab).await;
    cursor_to(f, "doc[1]").await;
}

/// Cells that carry an OSC 8 sequence when the screen is drawn at 120x40.
fn linked_cells(f: &Fixture) -> usize {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| f.screen.view(frame, &Theme::default()))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .filter(|c| c.symbol().starts_with("\u{1b}]8;;"))
        .count()
}

#[tokio::test]
async fn g_x_names_the_real_host_and_warns_about_text_before_the_at() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    set_links(
        &f,
        &[
            "https://bank.com@evil.example/login",
            "mailto:dana@example.com",
        ],
    );
    open_on_the_link_line(&mut f).await;
    keys(&mut f, "gx1").await;
    press(&mut f, KeyCode::Enter).await;
    let text = draw(&f, 90, 30);
    assert!(
        text.contains("https://bank.com@evil.example/login"),
        "{text}"
    );
    assert!(text.contains("Host: evil.example"), "{text}");
    assert!(text.contains("is not where this link goes"), "{text}");
    press(&mut f, KeyCode::Esc).await;
    keys(&mut f, "gx2").await;
    press(&mut f, KeyCode::Enter).await;
    let text = draw(&f, 90, 30);
    assert!(text.contains("To: dana@example.com"), "{text}");
    assert!(!text.contains("Host:"), "{text}");
    assert!(
        f.opened.lock().unwrap().is_empty(),
        "nothing opened without a yes"
    );
}

#[tokio::test]
async fn markers_are_plain_while_a_modal_is_open() {
    let mut f = fixture(true).await;
    store_bodies_on_fetch(&f);
    open_on_the_link_line(&mut f).await;
    assert_eq!(linked_cells(&f), 2, "hyperlinks with no modal");
    keys(&mut f, "gx").await;
    assert!(draw(&f, 120, 40).contains("Open link number (1-2)"));
    assert_eq!(linked_cells(&f), 0, "no OSC 8 cell under the link prompt");
    press(&mut f, KeyCode::Esc).await;
    assert_eq!(linked_cells(&f), 2, "hyperlinks again once it closes");
}
