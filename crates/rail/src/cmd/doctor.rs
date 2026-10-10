//! `rail doctor`: a health report over config, store, secrets and accounts.
//! Never panics on a broken setup and never prints token values.

use std::fmt::Write as _;
use std::path::PathBuf;

use rail_core::auth::{SecretStore, open_secret_store};
use rail_core::config::{self, Config, SecretsBackend};
use rail_core::paths::Paths;
use rail_core::store::{AccountRow, Store};

/// Address used only to ask the secret store whether it is reachable. Nothing is
/// ever stored under it.
const PROBE_ADDRESS: &str = "rail-doctor-probe@invalid";

#[derive(Debug)]
pub struct DoctorReport {
    pub config_path: PathBuf,
    /// Ok(number of accounts) or the error text.
    pub config: Result<usize, String>,
    pub db_path: PathBuf,
    pub db_size_bytes: Option<u64>,
    pub schema_version: Option<i64>,
    pub store_error: Option<String>,
    pub secrets_backend: &'static str,
    /// Set when the file backend is in use, so `render` can warn about it.
    pub secrets_file: Option<PathBuf>,
    pub secrets_error: Option<String>,
    /// False when the probe was skipped because the config did not load.
    pub secrets_checked: bool,
    pub accounts: Vec<AccountStatus>,
}

#[derive(Debug)]
pub struct AccountStatus {
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub has_tokens: bool,
    pub token_error: Option<String>,
    pub last_sync_at: Option<i64>,
}

impl DoctorReport {
    /// True when config, store and secrets all check out. The file-backend
    /// warning does not affect health.
    pub fn is_healthy(&self) -> bool {
        self.config.is_ok()
            && self.store_error.is_none()
            && self.secrets_error.is_none()
            && self.accounts.iter().all(|a| a.token_error.is_none())
    }
}

/// Collects the report. Doctor opens the store, creating it if absent, so a
/// first run leaves an empty database behind.
pub fn gather(paths: &Paths) -> DoctorReport {
    let config_path = paths.config_file();
    let db_path = paths.db_file();
    let loaded = config::load(&config_path);
    let backend = loaded
        .as_ref()
        .map_or(SecretsBackend::Keyring, |c| c.general.secrets);
    let secrets = open_secret_store(backend, paths);

    let mut report = DoctorReport {
        config_path,
        config: check_config(&loaded),
        db_size_bytes: None,
        db_path,
        schema_version: None,
        store_error: None,
        secrets_backend: secrets.backend_name(),
        secrets_file: matches!(backend, SecretsBackend::File).then(|| paths.secrets_file()),
        secrets_error: None,
        secrets_checked: false,
        accounts: Vec::new(),
    };
    let rows = check_store(&mut report);
    if let Ok(config) = &loaded {
        check_accounts(&mut report, config, secrets.as_ref(), &rows);
        check_secrets(&mut report, secrets.as_ref());
    }
    report
}

fn check_config(loaded: &Result<Config, config::ConfigError>) -> Result<usize, String> {
    loaded
        .as_ref()
        .map(|c| c.accounts.len())
        .map_err(ToString::to_string)
}

/// Opens the store, recording schema version, size and any failure.
fn check_store(report: &mut DoctorReport) -> Vec<AccountRow> {
    let store = match Store::open(&report.db_path) {
        Ok(store) => store,
        Err(e) => {
            report.store_error = Some(e.to_string());
            return Vec::new();
        }
    };
    report.db_size_bytes = std::fs::metadata(&report.db_path).ok().map(|m| m.len());
    match store.schema_version() {
        Ok(v) => report.schema_version = Some(v),
        Err(e) => report.store_error = Some(format!("reading schema version: {e}")),
    }
    match store.list_accounts() {
        Ok(rows) => rows,
        Err(e) => {
            report
                .store_error
                .get_or_insert_with(|| format!("listing accounts: {e}"));
            Vec::new()
        }
    }
}

fn check_accounts(
    report: &mut DoctorReport,
    config: &Config,
    secrets: &dyn SecretStore,
    rows: &[AccountRow],
) {
    for account in &config.accounts {
        let (has_tokens, token_error) = match secrets.get_tokens(&account.address) {
            Ok(t) => (t.is_some(), None),
            Err(e) => (false, Some(e.to_string())),
        };
        if let Some(err) = &token_error {
            report.secrets_error.get_or_insert_with(|| err.clone());
        }
        let last_sync_at = rows
            .iter()
            .find(|r| r.address.eq_ignore_ascii_case(&account.address))
            .and_then(|r| r.last_sync_at);
        report.accounts.push(AccountStatus {
            name: account.name.clone(),
            kind: account.kind.as_str(),
            address: account.address.clone(),
            has_tokens,
            token_error,
            last_sync_at,
        });
    }
}

/// Probes secret store access once, so a missing Secret Service shows up even
/// with zero accounts.
fn check_secrets(report: &mut DoctorReport, secrets: &dyn SecretStore) {
    report.secrets_checked = true;
    if let Err(e) = secrets.get_tokens(PROBE_ADDRESS) {
        report.secrets_error.get_or_insert_with(|| e.to_string());
    }
}

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
    let _ = match &report.config {
        Ok(n) => writeln!(
            out,
            "{} config   {} ({n} accounts)",
            mark(true),
            report.config_path.display()
        ),
        Err(e) => writeln!(out, "{} config   {e}", mark(false)),
    };
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
        Some(e) => writeln!(out, "{} store    {path}: {e}", mark(false)),
    };
}

fn render_secrets(out: &mut String, report: &DoctorReport) {
    let backend = report.secrets_backend;
    let _ = match (&report.secrets_error, &report.secrets_file) {
        _ if !report.secrets_checked && report.secrets_error.is_none() => writeln!(
            out,
            "{SKIP} secrets  {backend} (not checked: config not loaded)"
        ),
        (Some(e), _) => writeln!(out, "{} secrets  {backend}: {e}", mark(false)),
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
            (Some(e), _) => format!("error: {e}"),
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
            account.kind,
            tokens,
            last_sync
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(paths: &Paths, text: &str) {
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), text).unwrap();
    }

    const FILE_BACKEND_ONE_ACCOUNT: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n";

    #[test]
    fn doctor_reports_missing_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        let report = gather(&paths);
        assert!(!report.is_healthy());
        let text = render(&report);
        assert!(text.contains("rail account add"), "{text}");
        assert!(text.contains(&paths.config_file().display().to_string()));
        assert!(text.contains("skip secrets"), "{text}");
        assert!(!text.contains("ok   secrets"), "{text}");
    }

    #[test]
    fn doctor_reports_config_error_with_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, "[general]\nnope = 1\n");
        let report = gather(&paths);
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
        let report = gather(&paths);
        assert!(report.is_healthy(), "{}", render(&report));
        assert_eq!(report.schema_version, Some(1));
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].name, "work");
        assert!(!report.accounts[0].has_tokens);
        assert_eq!(report.secrets_backend, "file");
        let text = render(&report);
        assert!(text.contains("work"));
        assert!(text.contains("tokens: missing"));
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
        let report = gather(&paths);
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
        // A directory where the database file should be makes opening fail.
        std::fs::create_dir_all(paths.db_file()).unwrap();
        let report = gather(&paths);
        assert!(!report.is_healthy());
        assert!(report.store_error.is_some());
        assert!(render(&report).contains("FAIL store"));
    }

    #[test]
    fn corrupt_secrets_file_surfaces_as_secrets_fail() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        std::fs::create_dir_all(paths.secrets_file().parent().unwrap()).unwrap();
        std::fs::write(paths.secrets_file(), "not json {").unwrap();
        let report = gather(&paths);
        assert!(!report.is_healthy());
        assert!(render(&report).contains("FAIL secrets"));
    }

    #[test]
    fn list_accounts_failure_is_a_store_fail_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        write_config(&paths, FILE_BACKEND_ONE_ACCOUNT);
        // Open creates the schema; dropping the table makes list_accounts fail.
        let store = Store::open(&paths.db_file()).unwrap();
        store.conn().execute_batch("DROP TABLE accounts").unwrap();
        drop(store);
        let report = gather(&paths);
        let text = render(&report);
        assert!(!report.is_healthy(), "{text}");
        assert!(text.contains("FAIL store"), "{text}");
    }
}
