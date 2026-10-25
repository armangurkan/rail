//! Saved searches from outside Rail: `rail doctor` on a hand-written `[[saved_searches]]`
//! table (AC-22), and the whole save story in a pseudo-terminal — `/` a filter, `S`, a name,
//! `Enter` — with `config.toml` and the sidebar as the proof (AC-30).

use super::{StopsDaemon, WORK_FILE_BACKEND, rail_cmd, stdout_of, write_config};

/// `WORK_FILE_BACKEND` with the `[[saved_searches]]` tables in `tables` appended.
fn with_searches(tables: &str) -> String {
    format!("{WORK_FILE_BACKEND}{tables}")
}

const DANA: &str = "\n[[saved_searches]]\nname = \"dana\"\nquery = \"from:dana\"\n";

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

    use super::pty::{LIMIT, collect, drawn_row, script_available, text, wait_until};

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
