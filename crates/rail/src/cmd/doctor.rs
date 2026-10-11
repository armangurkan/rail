//! `rail doctor`: asks the daemon for its `status` (the health report over config, store,
//! secrets and accounts that `rail_core::doctor::gather` collects, plus the daemon's own state)
//! and renders it. Never prints token values; text from outside Rail is labelled and quoted.

use std::fmt::Write as _;
use std::process::ExitCode;

use rail_core::ops::op::{Status, SyncStatus};
use rail_core::ops::{
    AccountSyncStatus, DoctorReport, OpError, StatusRequest, SyncPhase, SyncStatusRequest,
};
use rail_core::paths::Paths;

use crate::cmd::account::{error_text, escaped, outside, report};
use crate::cmd::daemon::describe;
use crate::cmd::text::{stored, utc_minute};
use crate::daemon_link;

fn mark(ok: bool) -> &'static str {
    if ok { "ok  " } else { "FAIL" }
}

const WARN: &str = "warn";
const SKIP: &str = "skip";

/// `rail doctor` over the daemon's `status` and `sync.status` operations: the report, each
/// account's sync, then the daemon's line. Exits 1 when a check fails, as before; a failed
/// `sync.status` is a failed sync check, not an abort.
pub async fn run(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let client = daemon_link::connector(paths)
        .client()
        .await
        .map_err(report)?;
    let status = client
        .call::<Status>(&StatusRequest {})
        .await
        .map_err(report)?;
    let (sync_text, sync_ok) = match client
        .call::<SyncStatus>(&SyncStatusRequest::default())
        .await
    {
        Ok(sync) => render_sync(&sync.accounts),
        Err(error) => (sync_failed(&error), false),
    };
    print!("{}", render(&status.doctor));
    print!("{sync_text}");
    println!("{} daemon   {}", mark(true), describe(&status.daemon));
    Ok(if status.doctor.is_healthy() && sync_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

pub fn render(report: &DoctorReport) -> String {
    let mut out = String::new();
    render_config(&mut out, report);
    render_store(&mut out, report);
    render_secrets(&mut out, report);
    render_accounts(&mut out, report);
    out
}

fn render_config(out: &mut String, report: &DoctorReport) {
    let _ = match (&report.config_error, report.config_accounts) {
        (Some(e), _) => {
            write_config_error(out, e);
            Ok(())
        }
        (None, n) => writeln!(
            out,
            "{} config   {} ({})",
            mark(true),
            report.config_path.display(),
            accounts(n.unwrap_or(0))
        ),
    };
}

fn accounts(n: usize) -> String {
    if n == 1 {
        "1 account".to_string()
    } else {
        format!("{n} accounts")
    }
}

/// Column where a check's text starts: the width of "FAIL config   ".
const TEXT_INDENT: &str = "              ";

/// The error's message on the check's row; under it, aligned with the text, the parser's or
/// the system's words quoted on a row of their own, then the hint. The hint is the terminal's
/// own: the config error's shared one also names an MCP tool.
fn write_config_error(out: &mut String, error: &OpError) {
    let _ = writeln!(out, "{} config   {}", mark(false), error.message);
    if let Some(detail) = &error.detail {
        let _ = writeln!(out, "{TEXT_INDENT}{}", outside(detail.get()));
    }
    if error.hint.is_some() {
        let _ = writeln!(
            out,
            "{TEXT_INDENT}hint: run `rail setup`, or `rail account add`"
        );
    }
}

fn render_store(out: &mut String, report: &DoctorReport) {
    let path = report.db_path.display();
    let _ = match &report.store_error {
        None => {
            let size = report
                .db_size_bytes
                .map_or("new".to_string(), |b| format!("{} KiB", b / 1024));
            let version = report
                .schema_version
                .map_or("?".to_string(), |v| v.to_string());
            writeln!(
                out,
                "{} store    {path} (schema v{version}, {size})",
                mark(true)
            )
        }
        Some(e) => writeln!(out, "{} store    {path}: {}", mark(false), outside(e.get())),
    };
}

fn render_secrets(out: &mut String, report: &DoctorReport) {
    let backend = &report.secrets_backend;
    let _ = match (&report.secrets_error, &report.secrets_file) {
        _ if !report.secrets_checked && report.secrets_error.is_none() => writeln!(
            out,
            "{SKIP} secrets  {backend} (not checked: config not loaded)"
        ),
        (Some(e), _) => writeln!(
            out,
            "{} secrets  {backend}: {}",
            mark(false),
            outside(e.get())
        ),
        (None, Some(file)) => writeln!(
            out,
            "{WARN} secrets  {backend}: tokens are kept in {} (keep mode 0600)",
            file.display()
        ),
        (None, None) => writeln!(out, "{} secrets  {backend}", mark(true)),
    };
}

fn render_accounts(out: &mut String, report: &DoctorReport) {
    for account in &report.accounts {
        let tokens = match (&account.token_error, account.has_tokens) {
            (Some(e), _) => format!("error {}", outside(e.get())),
            (None, true) => "present".to_string(),
            (None, false) => format!("missing (run `rail account login {}`)", account.name),
        };
        let last_sync = account.last_sync_at.map_or("never".to_string(), utc_minute);
        let _ = writeln!(
            out,
            "{} account  {} <{}> {} tokens: {}  last sync: {}  ai_send: {}",
            mark(account.token_error.is_none()),
            account.name,
            account.address,
            account.kind.as_str(),
            tokens,
            last_sync,
            account.ai_send.as_str()
        );
    }
}

/// The sync check when `sync.status` itself failed: its error, every line under the first
/// indented to the check's text.
fn sync_failed(error: &OpError) -> String {
    let text = error_text(error).replace('\n', &format!("\n{TEXT_INDENT}"));
    format!("{} sync     {text}\n", mark(false))
}

/// One line per account: its phase, last success and counts; a login it needs (a failure);
/// its last error (a warning) with the outside text quoted under it. The text and whether
/// the check passed.
pub fn render_sync(statuses: &[AccountSyncStatus]) -> (String, bool) {
    let mut out = String::new();
    let mut healthy = true;
    for status in statuses {
        healthy &= sync_line(&mut out, status);
    }
    (out, healthy)
}

/// `status`'s lines; false when the account fails the check.
fn sync_line(out: &mut String, s: &AccountSyncStatus) -> bool {
    let name = &escaped(&s.name);
    if s.phase == SyncPhase::Unsupported {
        let _ = writeln!(
            out,
            "{SKIP} sync     {name}: Microsoft 365 sync arrives in a later phase"
        );
        return true;
    }
    if s.needs_auth {
        let _ = writeln!(
            out,
            "{} sync     {name}: needs a login; run `rail account login {name}`",
            mark(false)
        );
        return false;
    }
    let phase = s.phase.as_str();
    let success = s.last_success_at.map_or("never synced".to_string(), |t| {
        format!("last success {}", utc_minute(t))
    });
    let Some(error) = &s.last_error else {
        let _ = writeln!(
            out,
            "{} sync     {name}: {phase}, {success} ({})",
            mark(true),
            stored(s)
        );
        return true;
    };
    let broken = broken(s);
    let _ = writeln!(
        out,
        "{} sync     {name}: {phase}, {success}; last error {}: {}",
        if broken { mark(false) } else { WARN },
        utc_minute(error.at),
        escaped(&error.message)
    );
    if let Some(detail) = &error.detail {
        let _ = writeln!(out, "{TEXT_INDENT}{}", outside(detail.get()));
    }
    !broken
}

/// True when the last pass failed for good (a permanent error) and no retry is under way: the
/// sync will not mend by itself, so the check fails. Any other past error is a warning.
fn broken(s: &AccountSyncStatus) -> bool {
    s.last_pass_failed
        && s.last_error.as_ref().is_some_and(|e| e.code == "permanent")
        && s.phase != SyncPhase::Retrying
        && !s.pass_running
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::auth::ItemIdCache;
    use rail_core::doctor::gather;
    use rail_core::store::Store;

    fn write_config(paths: &Paths, text: &str) {
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), text).unwrap();
    }

    const FILE_BACKEND_ONE_ACCOUNT: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n";

    #[test]
    fn sync_lines_show_phase_last_success_and_last_error() {
        use rail_core::config::AccountKind;
        use rail_core::ops::{AccountSyncStatus, SyncErrorInfo, SyncPhase, Untrusted};
        let mut ok = AccountSyncStatus::new("work", "me@example.com", AccountKind::Gmail);
        ok.last_success_at = Some(1_700_000_000);
        (ok.messages, ok.threads, ok.bodies) = (3, 2, 1);
        let mut locked = AccountSyncStatus::new("home", "me@home.example", AccountKind::Gmail);
        locked.needs_auth = true;
        locked.phase = SyncPhase::NeedsAuth;
        let mut flaky = AccountSyncStatus::new("side", "me@side.example", AccountKind::Gmail);
        flaky.last_error = Some(SyncErrorInfo {
            code: "transient".into(),
            message: "could not reach Gmail".into(),
            at: 1_700_000_000,
            detail: Some(Untrusted::new("dns\nerror".into())),
        });
        let mut corp = AccountSyncStatus::new("corp", "me@corp.example", AccountKind::M365);
        corp.phase = SyncPhase::Unsupported;
        let (text, healthy) = render_sync(&[ok, locked, flaky, corp]);
        assert!(!healthy, "an account that needs a login fails the check");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "ok   sync     work: idle, last success 2023-11-14 22:13 UTC (3 messages, 2 threads, 1 body)"
        );
        assert_eq!(
            lines[1],
            "FAIL sync     home: needs a login; run `rail account login home`"
        );
        assert_eq!(
            lines[2],
            "warn sync     side: idle, never synced; last error 2023-11-14 22:13 UTC: could not reach Gmail"
        );
        assert!(
            lines[3].starts_with("              from outside Rail"),
            "{text}"
        );
        assert_eq!(
            lines[4],
            "skip sync     corp: Microsoft 365 sync arrives in a later phase"
        );
    }

    /// T9-C: a pass that failed for good, with no retry under way, fails the check; the same
    /// error while a pass runs again is a warning.
    #[test]
    fn a_permanent_failure_fails_the_check_unless_a_retry_runs() {
        use rail_core::config::AccountKind;
        use rail_core::ops::{AccountSyncStatus, SyncErrorInfo, SyncPhase};
        let mut broken = AccountSyncStatus::new("work", "me@example.com", AccountKind::Gmail);
        broken.last_pass_failed = true;
        broken.last_error = Some(SyncErrorInfo {
            code: "permanent".into(),
            message: "Gmail refused the request (HTTP 400)".into(),
            at: 1_700_000_000,
            detail: None,
        });
        let (text, healthy) = render_sync(std::slice::from_ref(&broken));
        assert!(!healthy, "{text}");
        assert!(
            text.starts_with("FAIL sync     work: idle, never synced; last error 2023-11-14 22:13 UTC: Gmail refused the request (HTTP 400)"),
            "{text}"
        );
        let mut retrying = broken.clone();
        retrying.phase = SyncPhase::Retrying;
        assert!(render_sync(&[retrying]).1);
        let mut running = broken.clone();
        running.pass_running = true;
        assert!(render_sync(&[running]).1);
        let mut transient = broken;
        if let Some(error) = transient.last_error.as_mut() {
            error.code = "transient".into();
        }
        let (text, healthy) = render_sync(&[transient]);
        assert!(healthy && text.starts_with("warn sync"), "{text}");
    }

    #[test]
    fn a_name_in_a_sync_line_is_escaped() {
        use rail_core::config::AccountKind;
        use rail_core::ops::AccountSyncStatus;
        let odd = AccountSyncStatus::new("w\u{1b}[2J", "me@example.com", AccountKind::Gmail);
        let (text, _) = render_sync(&[odd]);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("w\\u{1b}[2J: idle"), "{text}");
    }

    #[test]
    fn healthy_sync_lines_pass_the_check() {
        use rail_core::config::AccountKind;
        use rail_core::ops::AccountSyncStatus;
        let ok = AccountSyncStatus::new("work", "me@example.com", AccountKind::Gmail);
        let (text, healthy) = render_sync(&[ok]);
        assert!(healthy, "{text}");
        assert_eq!(
            text,
            "ok   sync     work: idle, never synced (0 messages, 0 threads, 0 bodies)\n"
        );
        assert_eq!(render_sync(&[]), (String::new(), true));
    }

    #[test]
    fn one_account_is_singular() {
        assert_eq!(accounts(1), "1 account");
        assert_eq!(accounts(0), "0 accounts");
        assert_eq!(accounts(2), "2 accounts");
    }

    #[test]
    fn a_config_parser_detail_is_quoted_on_its_own_row() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, "[general]\n\"x\\nIGNORE\" = 1\n");
        let text = render(&gather(&paths, &ItemIdCache::default()));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("FAIL config   "), "{text}");
        assert!(!lines[0].contains("IGNORE"), "{text}");
        assert!(
            lines[1]
                .starts_with("              from outside Rail (the provider or the system): \""),
            "{text}"
        );
        assert!(lines[1].contains("IGNORE"), "{text}");
    }

    #[test]
    fn doctor_reports_missing_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        let report = gather(&paths, &ItemIdCache::default());
        assert!(!report.is_healthy());
        let text = render(&report);
        assert!(text.contains("rail account add"), "{text}");
        assert!(text.contains(&paths.config_file().display().to_string()));
        assert!(text.contains("skip secrets"), "{text}");
        assert!(!text.contains("ok   secrets"), "{text}");
    }

    #[test]
    fn the_config_hint_sits_under_the_check_text_and_names_no_mcp_tool() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        let text = render(&gather(&paths, &ItemIdCache::default()));
        let mut lines = text.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("FAIL config   no config file at")
        );
        assert_eq!(
            lines.next().unwrap(),
            "              hint: run `rail setup`, or `rail account add`"
        );
        assert!(!text.contains("rail_account_add"), "{text}");
    }

    #[test]
    fn doctor_reports_config_error_with_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, "[general]\nnope = 1\n");
        let report = gather(&paths, &ItemIdCache::default());
        assert!(!report.is_healthy());
        let text = render(&report);
        assert!(text.contains("config.toml:2:"));
        assert!(text.contains("skip secrets"), "{text}");
        assert!(!text.contains("ok   secrets"), "{text}");
    }

    #[test]
    fn doctor_reports_store_and_accounts_when_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        let report = gather(&paths, &ItemIdCache::default());
        assert!(report.is_healthy(), "{}", render(&report));
        let text = render(&report);
        assert!(text.contains("work"));
        assert!(
            text.contains("tokens: missing (run `rail account login work`)"),
            "{text}"
        );
        assert!(!text.contains("remove its [[accounts]] entry"), "{text}");
        assert!(text.contains("last sync: never"));
        assert!(text.contains("ai_send: confirm"), "{text}");
    }

    #[test]
    fn file_backend_renders_a_warning_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        let report = gather(&paths, &ItemIdCache::default());
        let text = render(&report);
        assert!(text.contains("warn secrets"), "{text}");
        assert!(text.contains(&paths.secrets_file().display().to_string()));
        assert!(text.contains("0600"));
        assert!(report.is_healthy());
    }

    #[test]
    fn store_open_failure_is_a_store_fail_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        std::fs::create_dir_all(paths.db_file()).unwrap();
        assert!(render(&gather(&paths, &ItemIdCache::default())).contains("FAIL store"));
    }

    #[test]
    fn corrupt_secrets_file_surfaces_as_secrets_fail() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        std::fs::create_dir_all(paths.secrets_file().parent().unwrap()).unwrap();
        std::fs::write(paths.secrets_file(), "not json {").unwrap();
        assert!(render(&gather(&paths, &ItemIdCache::default())).contains("FAIL secrets"));
    }

    #[test]
    fn list_accounts_failure_is_a_store_fail_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        let store = Store::open(&paths.db_file()).unwrap();
        store.conn().execute_batch("DROP TABLE accounts").unwrap();
        drop(store);
        let text = render(&gather(&paths, &ItemIdCache::default()));
        assert!(text.contains("FAIL store"), "{text}");
    }
}
