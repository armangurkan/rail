//! Saved searches from outside Rail: `rail doctor` on a hand-written `[[saved_searches]]`
//! table (AC-22), and the whole save story in a pseudo-terminal — `/` a filter, `S`, a name,
//! `Enter` — with `config.toml` and the sidebar as the proof (AC-30).

use super::{StopsDaemon, WORK_FILE_BACKEND, rail_cmd, stdout_of, write_config};

/// `WORK_FILE_BACKEND` with the `[[saved_searches]]` tables in `tables` appended.
fn with_searches(tables: &str) -> String {
    format!("{WORK_FILE_BACKEND}{tables}")
}

const DANA: &str = "\n[[saved_searches]]\nname = \"dana\"\nquery = \"from:dana\"\n";

/// One hand-written saved search over fake Gmail's three threads (`Budget m1`, `Budget m2`,
/// `Budget m3`): `subject:` is a `LIKE` over the message subject (`store/filter.rs`), so this
/// one finds `Budget m1` alone. The name carries no digit, so "no count is drawn" can be read
/// off the sidebar row.
#[cfg(all(target_os = "linux", debug_assertions))]
const JUST_ONE: &str = "\n[[saved_searches]]\nname = \"just-one\"\nquery = \"subject:m1\"\n";

/// FR-20: the `[[accounts]]` table and the `[general]` key `WORK_FILE_BACKEND` puts in
/// `config.toml` are still there after Rail rewrote the file. Asserted line by line rather
/// than as one block: `toml_edit` keeps the tables it did not touch, not their spacing.
#[cfg(all(target_os = "linux", debug_assertions))]
fn keeps_the_account(text: &str) {
    for wanted in [
        "[[accounts]]",
        "name = \"work\"",
        "kind = \"gmail\"",
        "address = \"me@example.com\"",
        "client_id = \"cid\"",
        "client_secret = \"sec\"",
        "secrets = \"file\"",
    ] {
        assert!(text.contains(wanted), "{wanted} went missing:\n{text}");
    }
}

/// The terminal every pseudo-terminal test here asks `stty` for.
#[cfg(all(target_os = "linux", debug_assertions))]
const COLS: usize = 100;
#[cfg(all(target_os = "linux", debug_assertions))]
const ROWS: usize = 30;

/// The sidebar pane's slice of the drawn row holding `label`, trailing spaces trimmed: what
/// the sidebar shows for that row, without the panes beside it, its count at the end.
/// `pty::screen` gives each row as the terminal shows it; the sidebar has no left border and
/// its 25 columns of content are followed by the divider `│` at column 26
/// (`SIDEBAR_WIDTH = 26`, `screens/mail/layout.rs`), so the sidebar's cell is the text before
/// the row's first `│`. The header's cell is `▍Rail `, so the `Search: <name>` it carries once
/// a search is open can never be mistaken for a sidebar row.
///
/// A modal covers part of it: a centred box is 56 columns wide, so at 100 columns it starts at
/// column 22, inside the sidebar, and its border cuts this slice before the count at the
/// pane's right edge. So no count may be read off a row while a modal is open — the counts
/// test waits for the palette to close first.
#[cfg(all(target_os = "linux", debug_assertions))]
fn sidebar_slice(rows: &[String], label: &str) -> Option<String> {
    rows.iter()
        .filter_map(|row| row.split('│').next())
        .find(|cell| cell.contains(label))
        .map(|cell| cell.trim_end().to_string())
}

/// The report row holding `FAIL config`, and everything below it (the `detail` row, which
/// `outside()` wraps over as many rows as it needs).
fn config_failure(text: &str) -> (String, String) {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains("FAIL config"))
        .unwrap_or_else(|| panic!("no `FAIL config` row in {text:?}"));
    (lines[at].to_string(), lines[at + 1..].join("\n"))
}

/// AC-22 (FR-21): a valid saved search loads, and a bad or duplicate name stops `rail doctor`
/// with the offending name quoted on the detail row — never the query.
#[test]
fn doctor_accepts_a_saved_search_and_names_a_bad_one() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);

    write_config(&dir, &with_searches(DANA));
    let out = rail_cmd(&dir).arg("doctor").output().unwrap();
    let text = stdout_of(&out);
    // The config row, not the exit code: `WORK_FILE_BACKEND` has no tokens, so the sync check
    // fails and `rail doctor` exits 1 whatever the config holds — it exits 0 only when
    // `status.doctor.is_healthy() && sync_ok` (`cmd/doctor.rs`), and nothing in this feature
    // touches the sync check. `doctor_says_1_account` (`tests/cli.rs`) asserts the same row on
    // the same config, and its exit code not at all, for this reason.
    assert!(text.contains("ok   config"), "a saved search loads: {text}");
    assert!(text.contains("(1 account)"), "{text}");
    assert!(!text.contains("FAIL config"), "{text}");

    write_config(
        &dir,
        &with_searches("\n[[saved_searches]]\nname = \"two words\"\nquery = \"from:dana\"\n"),
    );
    let out = rail_cmd(&dir).arg("doctor").output().unwrap();
    let text = stdout_of(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    let (row, detail) = config_failure(&text);
    assert!(row.contains("config.toml"), "{text}");
    assert!(detail.contains("saved search name"), "{text}");
    assert!(detail.contains("two words"), "{text}");

    write_config(
        &dir,
        &with_searches(
            "\n[[saved_searches]]\nname = \"Dana\"\nquery = \"from:dana\"\n\n\
             [[saved_searches]]\nname = \"dana\"\nquery = \"from:dana\"\n",
        ),
    );
    let out = rail_cmd(&dir).arg("doctor").output().unwrap();
    let text = stdout_of(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    let (_, detail) = config_failure(&text);
    assert!(detail.contains("duplicate saved search name"), "{text}");
}

/// A store for `WORK_FILE_BACKEND`'s account holding one inbox thread from `tom@example.com`,
/// so `/from:tom` matches it. (`pty.rs`'s own `seed_html_thread` is module-private and is not
/// one of the ten items DELTA's ruling makes `pub(super)`.)
#[cfg(all(target_os = "linux", debug_assertions))]
fn seed_tom_thread(dir: &tempfile::TempDir) {
    use rail_core::provider::{Envelope, FolderKind, Mailbox, RemoteFolder};
    use rail_core::store::Store;
    use rail_core::sync::Batch;
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut store = Store::open(&data.join("rail.db")).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'me@example.com')",
            [],
        )
        .unwrap();
    let envelope = Envelope {
        remote_id: "m1".into(),
        thread_id: "t1".into(),
        subject: "Quarterly plan".into(),
        from: vec![Mailbox {
            name: Some("Tom".into()),
            address: "tom@example.com".into(),
        }],
        date: 1_760_000_000,
        internal_date: 1_760_000_000_000,
        folders: vec!["INBOX".into()],
        ..Envelope::default()
    };
    let batch = Batch {
        folders: Some(vec![RemoteFolder {
            parent: None,
            remote_id: "INBOX".into(),
            name: "Inbox".into(),
            kind: FolderKind::Inbox,
        }]),
        upserts: vec![envelope],
        ..Batch::new(1)
    };
    store.apply(&batch, 1_760_000_000).unwrap();
}

/// AC-30 (US-1/US-2, DELTA's end-to-end ruling): the real binary in a pseudo-terminal saves
/// the filter in force and draws it in the sidebar, and `config.toml` holds the table.
///
/// 100 columns **is** `SIDEBAR_MIN_COLUMNS`, so no `b` is needed, and `MailScreen` starts with
/// the list focused, the pane `list.save_search` is bound to, so no `Tab` is needed either.
/// The last wait carries the whole predicate: the prompt echoes the name before anything is
/// saved, so `needs-reply` alone would be vacuous, and the heading is drawn only after the
/// daemon's `Ok` — which it answers after writing the file.
#[cfg(all(target_os = "linux", debug_assertions))]
#[test]
fn saving_the_filter_writes_the_search_and_shows_it_in_the_sidebar() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::pty::{LIMIT, collect, drawn_row, screen, script_available, text, wait_until};

    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(&dir, WORK_FILE_BACKEND);
    seed_tom_thread(&dir);
    let _daemon = StopsDaemon(&dir);

    let command = format!(
        "stty cols 100 rows 30; exec '{}'",
        env!("CARGO_BIN_EXE_rail")
    );
    let mut script = Command::new("script");
    super::rail_env(&mut script, &dir)
        .args(["-q", "-e", "-c", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = super::KillsChild(Some(script.spawn().unwrap()));
    let (mut keys, output) = {
        let rail = child.0.as_mut().unwrap();
        let keys = rail.stdin.take().unwrap();
        let output = collect(rail.stdout.take().unwrap());
        (keys, output)
    };
    let seen = |needle: &'static str| {
        let output = output.clone();
        move || text(&output).contains(needle)
    };

    wait_until("the mail screen", &output, seen("All inboxes"));
    keys.write_all(b"/from:tom\r").unwrap();
    keys.flush().unwrap();
    wait_until("the filter in the header", &output, seen("from:tom"));
    keys.write_all(b"S").unwrap();
    keys.flush().unwrap();
    keys.write_all(b"needs-reply").unwrap();
    keys.flush().unwrap();
    keys.write_all(b"\r").unwrap();
    keys.flush().unwrap();
    let drawn = output.clone();
    wait_until("the saved search in the sidebar", &output, move || {
        let shown = text(&drawn);
        match (
            drawn_row(&shown, "SEARCHES"),
            drawn_row(&shown, "needs-reply"),
        ) {
            (Some(heading), Some(row)) => heading < row,
            _ => false,
        }
    });

    let saved = std::fs::read_to_string(&config).unwrap();
    assert!(saved.contains("[[saved_searches]]"), "{saved}");
    assert!(saved.contains("name = \"needs-reply\""), "{saved}");
    assert!(saved.contains("query = \"from:tom\""), "{saved}");
    // FR-20: the `[[accounts]]` table `write_config` put there survives Rail's write.
    keeps_the_account(&saved);
    // US-2 (FR-8): the row carries its unread count, and the one seeded thread is unread.
    // Read off `pty::screen`, which puts each row back together from the cells Rail drew.
    let counted = output.clone();
    wait_until("the search's unread count", &output, move || {
        sidebar_slice(&screen(&text(&counted), COLS, ROWS), "needs-reply")
            .is_some_and(|slice| slice.ends_with('1'))
    });

    keys.write_all(b"q").unwrap();
    keys.flush().unwrap();
    let deadline = Instant::now() + LIMIT;
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "rail did not quit:\n{:?}",
            text(&output)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(keys);
}

/// Plan C.3: the rest of the saved-search stories end to end — opening one as a mailbox, `r`,
/// `d`, the palette and `:search-counts` — the real binary in a pseudo-terminal against fake
/// Gmail. Every wait reads `pty::screen` rows, so what is asserted is what the terminal shows,
/// and every claim about the settings is read off `config.toml`. Gated like `mod pty`, whose
/// helpers these reuse together with `mod bulk`'s fake Gmail; `mod saved` itself is not gated,
/// because `rail doctor` runs everywhere.
#[cfg(all(target_os = "linux", debug_assertions))]
mod terminal {
    use std::path::{Path, PathBuf};
    use std::process::ChildStdin;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use wiremock::MockServer;

    use super::{COLS, JUST_ONE, ROWS, keeps_the_account, sidebar_slice, with_searches};
    use crate::pty::{LIMIT, screen, script_available, text, wait_until};
    use crate::{StopsDaemon, bulk, write_config};

    /// The header is the screen's first row.
    const HEADER: usize = 0;
    /// `j` enough times to pass the sidebar's last row: `Sidebar::step` "stays at either end"
    /// (`screens/mail/sidebar.rs`), so any number over the row count lands on the last row the
    /// cursor stops on.
    const PAST_THE_END: [u8; 30] = [b'j'; 30];

    /// Wait until the screen, as the 100x30 terminal shows it, passes `check` (rows from 0);
    /// on timeout fail with everything the terminal drew.
    fn wait_shown(
        what: &str,
        output: &Arc<Mutex<Vec<u8>>>,
        check: impl Fn(&[String]) -> bool + Send + 'static,
    ) {
        let drawn = output.clone();
        wait_until(what, output, move || {
            check(&screen(&text(&drawn), COLS, ROWS))
        });
    }

    /// Wait until the saved search `name` is the open mailbox holding only what its query
    /// finds: the header names it, `Budget m1` is drawn, and the other two threads are gone.
    /// All three are drawn before this, so the two that must leave carry the assertion.
    fn wait_for_the_one_thread(output: &Arc<Mutex<Vec<u8>>>, name: &'static str) {
        let title = format!("Search: {name}");
        wait_shown("the search open with its one thread", output, move |rows| {
            rows[HEADER].contains(&title)
                && rows.iter().any(|row| row.contains("Budget m1"))
                && !rows
                    .iter()
                    .any(|row| row.contains("Budget m2") || row.contains("Budget m3"))
        });
    }

    /// `rail` in a 100x30 pseudo-terminal against fake Gmail with `config` on disk, settled
    /// once all three threads are drawn: the child, its keyboard, everything it drew, and the
    /// path of `config.toml`.
    fn open_mail(
        dir: &tempfile::TempDir,
        server: &MockServer,
        config: &str,
    ) -> (bulk::Spawned, ChildStdin, Arc<Mutex<Vec<u8>>>, PathBuf) {
        let path = write_config(dir, config);
        bulk::store_tokens(dir);
        let (child, keys, output) = bulk::launch(dir, server);
        wait_shown("the three threads in the list", &output, |rows| {
            bulk::MESSAGES.iter().all(|id| {
                let subject = format!("Budget {id}");
                rows.iter().any(|row| row.contains(&subject))
            })
        });
        (child, keys, output, path)
    }

    /// `Tab` to the sidebar, then past its last row. FR-7 puts the saved searches last with no
    /// rule under them, so the cursor lands on the last saved search whatever the accounts
    /// above hold. A wrong row is never silent: `r` and `d` show FR-16's notice there and the
    /// wait that follows times out with the screen.
    fn to_the_last_search(keys: &mut ChildStdin) {
        bulk::type_keys(keys, b"\t");
        bulk::type_keys(keys, &PAST_THE_END);
    }

    /// `q`: Rail quits cleanly, as every pseudo-terminal test ends.
    fn quit(child: &mut bulk::Spawned, keys: &mut ChildStdin, output: &Arc<Mutex<Vec<u8>>>) {
        bulk::type_keys(keys, b"q");
        let status = bulk::wait_for_exit(child, output);
        assert!(status.success(), "{status:?}: {:?}", text(output));
    }

    /// `config.toml` as it is on disk.
    fn config_text(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// The hand-written saved search is still in `config.toml`, query and all: a read-only
    /// flow may not rewrite it.
    fn still_saved(path: &Path) {
        let saved = config_text(path);
        assert!(saved.contains("name = \"just-one\""), "{saved}");
        assert!(saved.contains("query = \"subject:m1\""), "{saved}");
        keeps_the_account(&saved);
    }

    /// US-4 (FR-10, FR-11): `Enter` on the search in the sidebar opens it as a mailbox — the
    /// header names it and the list holds only the thread its query finds. Nothing is written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_saved_search_opens_as_a_mailbox_with_only_its_threads() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let _daemon = StopsDaemon(&dir);
        let server = bulk::gmail().await;
        let (mut child, mut keys, output, config) =
            open_mail(&dir, &server, &with_searches(JUST_ONE));

        to_the_last_search(&mut keys);
        bulk::type_keys(&mut keys, b"\r");

        wait_for_the_one_thread(&output, "just-one");
        still_saved(&config);
        quit(&mut child, &mut keys, &output);
    }

    /// Wait until some row shows `needle` — a modal's box title, say.
    fn wait_anywhere(what: &str, output: &Arc<Mutex<Vec<u8>>>, needle: &'static str) {
        wait_shown(what, output, move |rows| {
            rows.iter().any(|row| row.contains(needle))
        });
    }

    /// US-5 (FR-14, FR-17, FR-20): `r` renames the search — the sidebar draws the new name and
    /// loses the old one, `config.toml` carries it with the query untouched, and the account
    /// table beside it survives the write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r_renames_the_saved_search_in_config_and_the_sidebar() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let _daemon = StopsDaemon(&dir);
        let server = bulk::gmail().await;
        let (mut child, mut keys, output, config) =
            open_mail(&dir, &server, &with_searches(JUST_ONE));

        to_the_last_search(&mut keys);
        bulk::type_keys(&mut keys, b"r");
        wait_anywhere("the rename prompt", &output, "Rename this saved search");
        // The prompt opens pre-filled with the name, the cursor at its end: eight Backspaces
        // (0x7f in raw mode) clear `just-one`, as the screen test's `press_n` does.
        bulk::type_keys(&mut keys, &[0x7f; 8]);
        bulk::type_keys(&mut keys, b"renamed\r");

        wait_shown("the rename in the sidebar", &output, |rows| {
            sidebar_slice(rows, "renamed").is_some() && sidebar_slice(rows, "just-one").is_none()
        });
        let saved = config_text(&config);
        assert!(saved.contains("name = \"renamed\""), "{saved}");
        assert!(!saved.contains("just-one"), "the old name is gone: {saved}");
        assert!(
            saved.contains("query = \"subject:m1\""),
            "the query is kept: {saved}"
        );
        keeps_the_account(&saved);
        quit(&mut child, &mut keys, &output);
    }

    /// US-5 (FR-15, FR-17, FR-20): `d` asks, `y` removes — the whole `SEARCHES` section leaves
    /// the sidebar (FR-7: with none saved there is no section), `config.toml` loses the table,
    /// and the account table survives the write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn d_then_y_removes_the_saved_search_from_config_and_the_sidebar() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let _daemon = StopsDaemon(&dir);
        let server = bulk::gmail().await;
        let (mut child, mut keys, output, config) =
            open_mail(&dir, &server, &with_searches(JUST_ONE));

        to_the_last_search(&mut keys);
        bulk::type_keys(&mut keys, b"d");
        wait_anywhere("the removal question", &output, "Remove a saved search");
        bulk::type_keys(&mut keys, b"y");

        wait_shown("the section gone from the sidebar", &output, |rows| {
            sidebar_slice(rows, "just-one").is_none() && sidebar_slice(rows, "SEARCHES").is_none()
        });
        let saved = config_text(&config);
        assert!(!saved.contains("[[saved_searches]]"), "{saved}");
        assert!(!saved.contains("just-one"), "{saved}");
        keeps_the_account(&saved);
        quit(&mut child, &mut keys, &output);
    }

    /// `modals/palette.rs`'s box title: on screen only while the palette is open.
    const PALETTE_TITLE: &str = "Command palette";

    /// US-3 (FR-9, FR-10): the palette lists the search as `Search: <name>` and running the
    /// entry opens it. The entry is proved by a row that is not the header — the header holds
    /// the same text once the search is open, so only its absence there makes the first wait
    /// about the palette.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_palette_lists_and_opens_a_saved_search() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let _daemon = StopsDaemon(&dir);
        let server = bulk::gmail().await;
        let (mut child, mut keys, output, config) =
            open_mail(&dir, &server, &with_searches(JUST_ONE));

        bulk::type_keys(&mut keys, b":search just-one");
        wait_shown("the palette's entry for the search", &output, |rows| {
            rows.iter().any(|row| row.contains(PALETTE_TITLE))
                && !rows[HEADER].contains("Search: just-one")
                && rows[HEADER + 1..]
                    .iter()
                    .any(|row| row.contains("Search: just-one"))
        });
        bulk::type_keys(&mut keys, b"\r");

        wait_for_the_one_thread(&output, "just-one");
        still_saved(&config);
        quit(&mut child, &mut keys, &output);
    }

    /// Wait until the search's row carries its unread count, or none, while `All inboxes`
    /// keeps its own: FR-19 turns off the saved searches' counts alone, so a frame caught half
    /// drawn — every count missing — can never be read as the setting having changed.
    fn wait_for_the_counts(output: &Arc<Mutex<Vec<u8>>>, counted: bool) {
        let what = if counted {
            "the search's unread count"
        } else {
            "no count on the search's row"
        };
        // Fake Gmail's threads are all unread, so `All inboxes` counts every one of them.
        let all_inboxes = bulk::MESSAGES.len().to_string();
        wait_shown(what, output, move |rows| {
            sidebar_slice(rows, "All inboxes").is_some_and(|slice| slice.ends_with(&all_inboxes))
                && sidebar_slice(rows, "just-one").is_some_and(|slice| {
                    if counted {
                        slice.ends_with('1')
                    } else {
                        !slice.chars().any(|c| c.is_ascii_digit())
                    }
                })
        });
    }

    /// How often `config.toml` is read while waiting for a write.
    const POLL: Duration = Duration::from_millis(50);
    /// Wait until `config.toml` passes `check`. `:search-counts` writes it on a blocking task
    /// (`screens/mail/themes.rs:205`), so the setting reaches the file after the redraw the
    /// screen waits on; bounded by `pty::LIMIT`, and failing with the file.
    fn wait_config(what: &str, path: &Path, check: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + LIMIT;
        loop {
            let text = config_text(path);
            if check(&text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; config.toml:\n{text}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// US-6 (FR-19, FR-20): `:search-counts` turns the sidebar's counts off and on and saves
    /// the setting, keeping everything else in `config.toml`. The count is read only once the
    /// palette has closed, because its box covers the cells the count is drawn in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn search_counts_toggles_and_is_saved_to_config() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let _daemon = StopsDaemon(&dir);
        let server = bulk::gmail().await;
        let (mut child, mut keys, output, config) =
            open_mail(&dir, &server, &with_searches(JUST_ONE));
        // Fake Gmail's three threads are unread, and the query finds one of them.
        wait_for_the_counts(&output, true);

        toggle_the_counts(&mut keys, &output);
        wait_for_the_counts(&output, false);
        wait_config("the setting off in config.toml", &config, |text| {
            text.contains("saved_search_counts = false")
        });
        still_saved(&config);

        toggle_the_counts(&mut keys, &output);
        wait_for_the_counts(&output, true);
        wait_config("the setting gone from config.toml", &config, |text| {
            !text.contains("saved_search_counts")
        });
        still_saved(&config);
        quit(&mut child, &mut keys, &output);
    }

    /// `:search-counts` from the palette, waiting for its box to leave the screen: while it is
    /// open its border cuts the sidebar before the count, so no count may be read until then.
    fn toggle_the_counts(keys: &mut ChildStdin, output: &Arc<Mutex<Vec<u8>>>) {
        bulk::type_keys(keys, b":search-counts\r");
        wait_shown("the palette closed", output, |rows| {
            !rows.iter().any(|row| row.contains(PALETTE_TITLE))
        });
    }
}
