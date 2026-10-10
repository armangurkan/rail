//! `rail doctor`: renders the health report over config, store, secrets and accounts that
//! `rail_core::doctor::gather` collects. Never prints token values.

use std::fmt::Write as _;

use rail_core::ops::{DoctorReport, OpError};

fn mark(ok: bool) -> &'static str {
    if ok { "ok  " } else { "FAIL" }
}

const WARN: &str = "warn";
const SKIP: &str = "skip";

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
            "{} config   {} ({} accounts)",
            mark(true),
            report.config_path.display(),
            n.unwrap_or(0)
        ),
    };
}

/// Column where a check's text starts: the width of "FAIL config   ".
const TEXT_INDENT: &str = "              ";

/// The error's message and detail on the check's row, its hint on a row under it, aligned with
/// the text. The hint is the terminal's own: the config error's shared one also names an MCP
/// tool.
fn write_config_error(out: &mut String, error: &OpError) {
    let mut line = error.message.clone();
    if let Some(detail) = &error.detail {
        line = format!("{line}: {}", detail.get());
    }
    let _ = writeln!(out, "{} config   {line}", mark(false));
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
        Some(e) => writeln!(out, "{} store    {path}: {}", mark(false), e.get()),
    };
}

fn render_secrets(out: &mut String, report: &DoctorReport) {
    let backend = &report.secrets_backend;
    let _ = match (&report.secrets_error, &report.secrets_file) {
        _ if !report.secrets_checked && report.secrets_error.is_none() => writeln!(
            out,
            "{SKIP} secrets  {backend} (not checked: config not loaded)"
        ),
        (Some(e), _) => writeln!(out, "{} secrets  {backend}: {}", mark(false), e.get()),
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
            (Some(e), _) => format!("error: {}", e.get()),
            (None, true) => "present".to_string(),
            (None, false) => format!("missing (run `rail account login {}`)", account.name),
        };
        let last_sync = account
            .last_sync_at
            .map_or("never".to_string(), |t| format!("unix {t}"));
        let _ = writeln!(
            out,
            "{} account  {} <{}> {} tokens: {}  last sync: {}",
            mark(account.token_error.is_none()),
            account.name,
            account.address,
            account.kind.as_str(),
            tokens,
            last_sync
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::auth::ItemIdCache;
    use rail_core::doctor::gather;
    use rail_core::paths::Paths;
    use rail_core::store::Store;

    fn write_config(paths: &Paths, text: &str) {
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), text).unwrap();
    }

    const FILE_BACKEND_ONE_ACCOUNT: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n";

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
