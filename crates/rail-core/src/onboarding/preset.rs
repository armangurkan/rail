//! Organisation presets: TOML files that prefill the onboarding wizard. Rail reads them and
//! never writes them, and they never hold tokens.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

use crate::config::load::line_col;
use crate::config::{AccountKind, ConfigError};
use crate::error::ErrorClass;
use crate::guarded::{GuardedReadError, read_capped};
use crate::paths::Paths;

/// Where an administrator installs presets for every user of a machine.
pub const SYSTEM_PRESET_DIR: &str = "/etc/rail/presets";
/// The largest preset body fetched from a URL.
pub const MAX_PRESET_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// Shown in the wizard's preset list.
    #[serde(default)]
    pub name: Option<String>,
    pub kind: AccountKind,
    pub client_id: String,
    /// Microsoft only.
    #[serde(default)]
    pub tenant: Option<String>,
    /// Gmail desktop apps need one; Microsoft public clients do not.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// An optional "Need help?" link.
    #[serde(default)]
    pub help_url: Option<String>,
}

impl std::fmt::Debug for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preset")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("client_id", &self.client_id)
            .field("tenant", &self.tenant)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("help_url", &self.help_url)
            .finish()
    }
}

impl Preset {
    /// The name shown in the wizard, falling back to kind and client id.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("{} ({})", self.kind.as_str(), self.client_id))
    }
}

/// Parse and validate preset text. `path` (a file path or the URL it came from) is only used
/// in error messages, which carry its line and column exactly like `config.toml` errors.
pub fn parse_preset(text: &str, path: &Path) -> Result<Preset, ConfigError> {
    let preset: Preset = toml::from_str(text).map_err(|e| {
        let (line, col) = e.span().map_or((0, 0), |s| line_col(text, s.start));
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: parse_message(text, line, e.message()),
        }
    })?;
    validate(&preset).map_err(|message| ConfigError::Invalid {
        path: path.to_path_buf(),
        message,
    })?;
    Ok(preset)
}

/// serde echoes a mistyped value in its message ("invalid type: integer `5`"), so an error on
/// the `client_secret` line gets a fixed message instead of the parser's.
fn parse_message(text: &str, line: usize, message: &str) -> String {
    let on_secret_line = text.lines().nth(line.saturating_sub(1)).is_some_and(|l| {
        l.trim_start()
            .trim_start_matches('"')
            .starts_with("client_secret")
    });
    if on_secret_line {
        "client_secret must be a string".to_string()
    } else {
        message.to_string()
    }
}

fn validate(preset: &Preset) -> Result<(), String> {
    if preset.client_id.trim().is_empty() {
        return Err("client_id must not be empty".to_string());
    }
    if preset.kind == AccountKind::Gmail && preset.tenant.is_some() {
        return Err("tenant applies only to kind = \"m365\"".to_string());
    }
    if let Some(help) = &preset.help_url {
        let https = Url::parse(help).is_ok_and(|u| u.scheme() == "https");
        if !https {
            return Err(format!("help_url {help:?} must be an https:// URL"));
        }
    }
    Ok(())
}

/// What `read_preset` says about a file over `MAX_PRESET_BYTES`.
pub const PRESET_TOO_LARGE: &str = "the preset is larger than 64 KiB";

/// Read and parse a preset file, refusing anything over 64 KiB unread, like `fetch_preset`.
pub fn read_preset(path: &Path) -> Result<Preset, ConfigError> {
    let invalid = |message: &str| ConfigError::Invalid {
        path: path.to_path_buf(),
        message: message.to_string(),
    };
    let bytes = read_capped(path, MAX_PRESET_BYTES as u64).map_err(|e| match e {
        GuardedReadError::NotRegular => invalid("the preset is not a regular file"),
        GuardedReadError::TooLarge => invalid(PRESET_TOO_LARGE),
        GuardedReadError::Io(source) => ConfigError::Io {
            path: path.to_path_buf(),
            source,
        },
    })?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("the preset is not UTF-8 text"))?;
    parse_preset(&text, path)
}

/// The directories searched for `*.toml` presets, in order: the user's, then the system's.
pub fn preset_dirs(paths: &Paths) -> Vec<PathBuf> {
    vec![
        paths.config_dir.join("presets"),
        PathBuf::from(SYSTEM_PRESET_DIR),
    ]
}

pub fn discover_presets(paths: &Paths) -> Vec<(PathBuf, Result<Preset, ConfigError>)> {
    discover_presets_in(&preset_dirs(paths))
}

/// Every `*.toml` file in `dirs`, sorted by name within each directory. A missing directory
/// is skipped; an unreadable one or a bad file is reported in the list, never dropped.
pub fn discover_presets_in(dirs: &[PathBuf]) -> Vec<(PathBuf, Result<Preset, ConfigError>)> {
    dirs.iter().flat_map(|dir| scan_dir(dir)).collect()
}

fn scan_dir(dir: &Path) -> Vec<(PathBuf, Result<Preset, ConfigError>)> {
    let io_error = |source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![(dir.to_path_buf(), Err(io_error(e)))],
    };
    let mut files = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => files.push(entry.path()),
            Err(e) => return vec![(dir.to_path_buf(), Err(io_error(e)))],
        }
    }
    files.retain(|p| p.extension().is_some_and(|ext| ext == "toml"));
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let preset = read_preset(&path);
            (path, preset)
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error(
        "refusing to fetch the preset from {url}: it must be an https:// URL, because a preset fetched over plain http could be altered by anyone on the network"
    )]
    NotHttps { url: String },
    #[error("{url:?} is not a valid URL: {message}")]
    BadUrl { url: String, message: String },
    #[error("fetching the preset from {url} failed: {message}")]
    Network { url: String, message: String },
    #[error("fetching the preset from {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("the preset at {url} is larger than 64 KiB")]
    TooLarge { url: String },
    #[error(transparent)]
    Parse(#[from] ConfigError),
}

impl PresetError {
    /// Network trouble, 5xx and 429 may clear up; everything else needs the URL or file fixed.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Network { .. } => ErrorClass::Transient,
            Self::Status { status, .. } if *status >= 500 || *status == 429 => {
                ErrorClass::Transient
            }
            _ => ErrorClass::Permanent,
        }
    }
}

/// Fetch a preset over https with the shared HTTP client (its timeouts, no redirects). A plain
/// http URL is refused before any request is made. The body is capped at 64 KiB and parsed
/// exactly like a local file.
pub async fn fetch_preset(http: &reqwest::Client, url: &str) -> Result<Preset, PresetError> {
    let parsed = Url::parse(url).map_err(|e| PresetError::BadUrl {
        url: url.to_string(),
        message: e.to_string(),
    })?;
    if parsed.scheme() != "https" {
        return Err(PresetError::NotHttps {
            url: url.to_string(),
        });
    }
    fetch_unchecked(http, parsed).await
}

/// The fetch without the https check, so tests can use wiremock's plain-http server.
pub(crate) async fn fetch_unchecked(
    http: &reqwest::Client,
    url: Url,
) -> Result<Preset, PresetError> {
    let shown = url.to_string();
    let network = |e: reqwest::Error| PresetError::Network {
        url: shown.clone(),
        message: e.to_string(),
    };
    let mut response = http.get(url).send().await.map_err(network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(PresetError::Status {
            url: shown.clone(),
            status: status.as_u16(),
        });
    }
    let too_large = || PresetError::TooLarge { url: shown.clone() };
    if response
        .content_length()
        .is_some_and(|n| n > MAX_PRESET_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_PRESET_BYTES {
            return Err(too_large());
        }
    }
    let text = String::from_utf8(body).map_err(|_| {
        PresetError::Parse(ConfigError::Invalid {
            path: PathBuf::from(&shown),
            message: "the preset is not UTF-8 text".to_string(),
        })
    })?;
    Ok(parse_preset(&text, Path::new(&shown))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http_client;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CONTOSO: &str = r#"name = "Contoso Microsoft 365"
kind = "m365"
client_id = "00000000-0000-0000-0000-000000000000"
tenant = "contoso.onmicrosoft.com"
help_url = "https://intranet.example/rail"
"#;

    const ACME_GMAIL: &str = "name = \"Acme Gmail\"\nkind = \"gmail\"\nclient_id = \"acme.apps.googleusercontent.com\"\nclient_secret = \"GOCSPX-acme\"\n";

    fn p(text: &str) -> Result<Preset, ConfigError> {
        parse_preset(text, Path::new("/etc/rail/presets/acme.toml"))
    }

    #[test]
    fn parses_the_documented_example() {
        let preset = p(CONTOSO).unwrap();
        assert_eq!(preset.name.as_deref(), Some("Contoso Microsoft 365"));
        assert_eq!(preset.kind, AccountKind::M365);
        assert_eq!(preset.client_id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(preset.tenant.as_deref(), Some("contoso.onmicrosoft.com"));
        assert_eq!(preset.client_secret, None);
        assert_eq!(
            preset.help_url.as_deref(),
            Some("https://intranet.example/rail")
        );
        assert_eq!(preset.label(), "Contoso Microsoft 365");
    }

    #[test]
    fn unknown_key_reports_path_and_line() {
        let err = p("kind = \"gmail\"\nclient_id = \"x\"\nclient_scret = \"y\"\n").unwrap_err();
        match &err {
            ConfigError::Parse { line, message, .. } => {
                assert_eq!(*line, 3, "{message}");
                assert!(message.contains("client_scret"), "{message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
        assert!(
            err.to_string()
                .starts_with("/etc/rail/presets/acme.toml:3:"),
            "{err}"
        );
    }

    #[test]
    fn wrong_type_reports_its_line() {
        let err = p("kind = \"gmail\"\nclient_id = 5\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 2, .. }), "{err}");
    }

    #[test]
    fn missing_kind_and_unknown_kind_are_rejected() {
        let missing = p("client_id = \"x\"\n").unwrap_err();
        assert!(matches!(missing, ConfigError::Parse { .. }), "{missing}");
        assert!(missing.to_string().contains("kind"), "{missing}");
        let unknown = p("kind = \"imap\"\nclient_id = \"x\"\n").unwrap_err();
        assert!(
            matches!(unknown, ConfigError::Parse { line: 1, .. }),
            "{unknown}"
        );
    }

    #[test]
    fn invalid_values_are_rejected_with_the_path() {
        let cases = [
            ("kind = \"gmail\"\nclient_id = \" \"\n", "client_id"),
            (
                "kind = \"gmail\"\nclient_id = \"x\"\ntenant = \"t\"\n",
                "tenant",
            ),
            (
                "kind = \"m365\"\nclient_id = \"x\"\nhelp_url = \"http://intranet\"\n",
                "help_url",
            ),
        ];
        for (text, needle) in cases {
            let err = p(text).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
            assert!(err.to_string().contains(needle), "{err}");
            assert!(
                err.to_string().starts_with("/etc/rail/presets/acme.toml"),
                "{err}"
            );
        }
    }

    #[test]
    fn debug_hides_the_client_secret() {
        let preset = p(ACME_GMAIL).unwrap();
        assert!(
            preset.client_secret.is_some(),
            "client_secret was not parsed"
        );
        let debug = format!("{preset:?}");
        assert!(!debug.contains("GOCSPX-acme"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn mistyped_secret_error_does_not_echo_the_value() {
        const SECRET_VALUE: &str = "987654321";
        let text = format!("kind = \"gmail\"\nclient_id = \"x\"\nclient_secret = {SECRET_VALUE}\n");
        let err = p(&text).unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { line: 3, .. }),
            "expected a Parse error on line 3"
        );
        assert!(
            !err.to_string().contains(SECRET_VALUE),
            "error message echoed the secret"
        );
    }

    #[test]
    fn label_falls_back_to_kind_and_client_id() {
        let preset = p("kind = \"gmail\"\nclient_id = \"abc\"\nclient_secret = \"s\"\n").unwrap();
        assert_eq!(preset.label(), "gmail (abc)");
    }

    #[test]
    fn discovery_reads_toml_files_in_order_and_reports_bad_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(first.join("b.toml"), ACME_GMAIL).unwrap();
        std::fs::write(first.join("a.toml"), "kind = \"nope\"\n").unwrap();
        std::fs::write(first.join("notes.txt"), "not a preset").unwrap();
        let missing = tmp.path().join("missing");
        let found = discover_presets_in(&[first.clone(), missing]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, first.join("a.toml"));
        assert!(found[0].1.is_err());
        assert_eq!(found[1].0, first.join("b.toml"));
        assert_eq!(found[1].1.as_ref().unwrap().label(), "Acme Gmail");
    }

    #[test]
    fn preset_dirs_are_the_config_presets_dir_then_etc() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        assert_eq!(
            preset_dirs(&paths),
            vec![
                tmp.path().join("config").join("presets"),
                PathBuf::from("/etc/rail/presets"),
            ]
        );
    }

    #[test]
    fn a_preset_file_over_64_kib_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big.toml");
        let mut text = ACME_GMAIL.to_string();
        text.push_str(&"#".repeat(65 * 1024));
        std::fs::write(&file, text).unwrap();
        let err = read_preset(&file).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { message, .. } if message == PRESET_TOO_LARGE),
            "{err}"
        );
    }

    /// `ACME_GMAIL` padded with a comment line to exactly `total` bytes.
    fn padded_preset(total: usize) -> String {
        let mut text = ACME_GMAIL.to_string();
        text.push('#');
        let pad = total - text.len();
        text.push_str(&" ".repeat(pad));
        assert_eq!(text.len(), total);
        text
    }

    #[test]
    fn a_preset_file_of_exactly_64_kib_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("edge.toml");
        std::fs::write(&file, padded_preset(MAX_PRESET_BYTES)).unwrap();
        assert!(read_preset(&file).is_ok());
    }

    #[test]
    fn a_preset_file_one_byte_over_64_kib_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("over.toml");
        std::fs::write(&file, padded_preset(MAX_PRESET_BYTES + 1)).unwrap();
        let err = read_preset(&file).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { message, .. } if message == PRESET_TOO_LARGE),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_preset_path_that_is_not_a_regular_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_preset(tmp.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        let message = crate::ops::preset_file_message(&err);
        assert!(message.ends_with("not a valid Rail preset"), "{message}");
    }

    #[test]
    fn read_preset_refuses_a_fifo_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("pipe.toml");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let (done, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(read_preset(&fifo).map(|_| ()).map_err(|e| e.to_string()));
        });
        let err = answer
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a FIFO must be refused without blocking")
            .unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn read_preset_reports_a_missing_file_as_io() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_preset(&tmp.path().join("nope.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "{err}");
    }

    async fn serve(body: &str, status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rail/preset.toml"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    fn url(server: &MockServer) -> Url {
        Url::parse(&format!("{}/rail/preset.toml", server.uri())).unwrap()
    }

    #[tokio::test]
    async fn fetch_parses_a_served_preset() {
        let server = serve(CONTOSO, 200).await;
        let preset = fetch_unchecked(&http_client().unwrap(), url(&server))
            .await
            .unwrap();
        assert_eq!(preset.kind, AccountKind::M365);
        assert_eq!(preset.tenant.as_deref(), Some("contoso.onmicrosoft.com"));
    }

    #[tokio::test]
    async fn fetch_rejects_a_body_over_64_kib() {
        let server = serve(&"#".repeat(MAX_PRESET_BYTES + 1), 200).await;
        let err = fetch_unchecked(&http_client().unwrap(), url(&server))
            .await
            .unwrap_err();
        assert!(matches!(err, PresetError::TooLarge { .. }), "{err}");
        assert_eq!(err.class(), ErrorClass::Permanent);
    }

    #[tokio::test]
    async fn fetch_reports_server_errors_as_transient() {
        let server = serve("", 503).await;
        let err = fetch_unchecked(&http_client().unwrap(), url(&server))
            .await
            .unwrap_err();
        assert!(
            matches!(err, PresetError::Status { status: 503, .. }),
            "{err}"
        );
        assert_eq!(err.class(), ErrorClass::Transient);
        let server = serve("", 404).await;
        let err = fetch_unchecked(&http_client().unwrap(), url(&server))
            .await
            .unwrap_err();
        assert_eq!(err.class(), ErrorClass::Permanent);
    }

    #[tokio::test]
    async fn fetch_reports_a_bad_body_with_the_url_and_line() {
        let server = serve("kind = \"gmail\"\nbogus = 1\n", 200).await;
        let err = fetch_unchecked(&http_client().unwrap(), url(&server))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("/rail/preset.toml:2:"), "{text}");
    }

    #[tokio::test]
    async fn fetch_refuses_plain_http_before_any_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CONTOSO))
            .expect(0)
            .mount(&server)
            .await;
        let plain = format!("{}/rail/preset.toml", server.uri());
        assert!(plain.starts_with("http://"));
        let err = fetch_preset(&http_client().unwrap(), &plain)
            .await
            .unwrap_err();
        assert!(matches!(err, PresetError::NotHttps { .. }), "{err}");
        assert!(err.to_string().contains("https://"), "{err}");
        assert_eq!(err.class(), ErrorClass::Permanent);
        server.verify().await;
    }

    #[tokio::test]
    async fn fetch_rejects_an_unparseable_url() {
        let err = fetch_preset(&http_client().unwrap(), "not a url")
            .await
            .unwrap_err();
        assert!(matches!(err, PresetError::BadUrl { .. }), "{err}");
    }
}
