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

/// Open the thread, open m1's card and put the cursor on its line with both markers.
async fn open_on_the_link_line(f: &mut Fixture) {
    press(f, KeyCode::Enter).await;
    press(f, KeyCode::Home).await;
    press(f, KeyCode::Char('n')).await;
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

/// Open link `which` (1-based) of `links` and draw the confirmation at `cols` x `rows`.
async fn confirmation(links: &[&str], which: char, cols: u16, rows: u16) -> String {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    set_links(&f, links);
    open_on_the_link_line(&mut f).await;
    keys(&mut f, &format!("gx{which}")).await;
    press(&mut f, KeyCode::Enter).await;
    draw(&f, cols, rows)
}

fn long_address() -> String {
    format!("https://bank.com@evil.example/{}", "a".repeat(1_900))
}

#[tokio::test]
async fn a_long_address_cannot_hide_the_host_the_warning_or_the_hint() {
    let long = long_address();
    for (cols, rows) in [(80, 24), (40, 12), (30, 10)] {
        let text = confirmation(&[&long, "https://x.example/"], '1', cols, rows).await;
        assert!(text.contains("Warning:"), "{cols}x{rows}\n{text}");
        assert!(text.contains("evil.example"), "{cols}x{rows}\n{text}");
        assert!(text.contains("y yes"), "{cols}x{rows}\n{text}");
    }
    let text = confirmation(&[&long, "https://x.example/"], '1', 80, 24).await;
    assert!(text.contains("Host: evil.example"), "{text}");
}

#[tokio::test]
async fn a_long_host_shows_its_real_end_and_a_flood_of_recipients_is_counted() {
    let label = "a".repeat(63);
    let host = format!(
        "https://bank.com@bank.com.secure.{}.evil.example/",
        [&label; 27].map(String::as_str).join(".")
    );
    let text = confirmation(&[&host, "https://x.example/"], '1', 80, 24).await;
    assert!(
        text.contains("Warning:") && text.contains("evil.example"),
        "{text}"
    );
    assert!(text.contains("y yes"), "{text}");
    let bcc: String = (0..190).map(|_| "&bcc=u@x.y").collect();
    let mail = format!("mailto:a@b.example?subject=s{bcc}");
    let text = confirmation(&["https://x.example/", &mail], '2', 80, 24).await;
    assert!(text.contains("adds recipients"), "{text}");
    assert!(text.contains("more recipients"), "{text}");
    assert!(text.contains("y yes"), "{text}");
}
