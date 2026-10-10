//! Where a client secret comes from: a value, Google's downloaded JSON file, a 1Password
//! reference, or a preset. A JSON file may be any path the daemon can read, so its contents
//! never appear in an error, a log line or a response.

use std::path::Path;

use serde::Deserialize;

use super::{blocking, is_url, require_absolute};
use crate::auth::OpCli;
use crate::auth::onepassword::validate_reference;
use crate::error::ErrorClass;
use crate::guarded::{GuardedReadError, read_capped};
use crate::onboarding::{Preset, PresetError, fetch_preset, read_preset};
use crate::ops::{ClientSecretInput, ErrorCode, OP_REFERENCE_PREFIX, OpError, Secret};

/// The largest client secret file Rail reads. Google's are under 1 KiB.
pub const MAX_CLIENT_FILE_BYTES: u64 = 64 * 1024;
const NOT_GOOGLE_JSON: &str = "not a Google client secret JSON file";

/// What a client secret input resolved to. `Debug` shows only which parts are present.
#[derive(Default)]
pub struct ResolvedSecret {
    /// From a Google JSON file or a preset.
    pub client_id: Option<String>,
    /// From a preset.
    pub tenant: Option<String>,
    /// The value the OAuth client uses for this login.
    pub login: Option<Secret>,
    /// What goes into config.toml: the value, or the `op://` reference.
    pub stored: Option<Secret>,
}

impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecret")
            .field("client_id", &self.client_id)
            .field("tenant", &self.tenant)
            .field("login", &self.login.as_ref().map(|_| "<redacted>"))
            .field("stored", &self.stored.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Resolve `input`. File and `op` work runs on the blocking pool.
pub async fn resolve_client_secret(
    input: Option<&ClientSecretInput>,
    http: &reqwest::Client,
    op: &OpCli,
) -> Result<ResolvedSecret, OpError> {
    match input {
        None => Ok(ResolvedSecret::default()),
        Some(ClientSecretInput::Value(value))
            if value.expose().starts_with(OP_REFERENCE_PREFIX) =>
        {
            let (op, reference) = (op.clone(), value.expose().to_string());
            blocking(move || resolve_reference(&op, &reference)).await
        }
        Some(ClientSecretInput::Value(value)) => Ok(ResolvedSecret {
            login: Some(value.clone()),
            stored: Some(value.clone()),
            ..ResolvedSecret::default()
        }),
        Some(ClientSecretInput::JsonFile(path)) => {
            require_absolute(path)?;
            let path = path.clone();
            blocking(move || read_google_client_file(&path)).await
        }
        Some(ClientSecretInput::Reference(reference)) => {
            let (op, reference) = (op.clone(), reference.clone());
            blocking(move || resolve_reference(&op, &reference)).await
        }
        Some(ClientSecretInput::Preset(source)) => preset_secret(source, http).await,
    }
}

fn resolve_reference(op: &OpCli, reference: &str) -> Result<ResolvedSecret, OpError> {
    validate_reference(reference)?;
    let value = op.read_reference(reference)?;
    Ok(ResolvedSecret {
        login: Some(value),
        stored: Some(Secret::new(reference)),
        ..ResolvedSecret::default()
    })
}

async fn preset_secret(source: &str, http: &reqwest::Client) -> Result<ResolvedSecret, OpError> {
    let preset: Preset = if is_url(source) {
        fetch_preset(http, source).await?
    } else {
        let path = Path::new(source).to_path_buf();
        require_absolute(&path)?;
        blocking(move || read_preset(&path).map_err(|e| PresetError::Parse(e).into())).await?
    };
    // A remote preset must not plant a reference that a later login would resolve.
    if preset
        .client_secret
        .as_deref()
        .is_some_and(|s| s.starts_with(OP_REFERENCE_PREFIX))
    {
        return Err(OpError::new(
            ErrorCode::Preset,
            ErrorClass::Permanent,
            "a preset must not carry a 1Password reference",
        ));
    }
    let secret = preset.client_secret.map(Secret::new);
    Ok(ResolvedSecret {
        client_id: Some(preset.client_id),
        tenant: preset.tenant,
        login: secret.clone(),
        stored: secret,
    })
}

#[derive(Deserialize)]
struct GoogleClientFile {
    installed: Option<GoogleClient>,
    web: Option<GoogleClient>,
}

#[derive(Deserialize)]
struct GoogleClient {
    client_id: String,
    client_secret: String,
}

/// Read Google's `client_secret_*.json` (`{"installed": {"client_id", "client_secret", ...}}`).
/// Every failure names the path and a fixed reason, never a byte of the file.
pub(crate) fn read_google_client_file(path: &Path) -> Result<ResolvedSecret, OpError> {
    let refuse = |reason: &str| OpError::invalid_input(format!("{}: {reason}", path.display()));
    let bytes = read_capped(path, MAX_CLIENT_FILE_BYTES).map_err(|e| match e {
        GuardedReadError::NotRegular => {
            refuse(&format!("is not a regular file; {NOT_GOOGLE_JSON}"))
        }
        GuardedReadError::TooLarge => refuse(&format!("larger than 64 KiB; {NOT_GOOGLE_JSON}")),
        GuardedReadError::Io(e) => refuse(&format!("cannot be read ({})", e.kind())),
    })?;
    let parsed: GoogleClientFile =
        serde_json::from_slice(&bytes).map_err(|_| refuse(NOT_GOOGLE_JSON))?;
    let client = parsed
        .installed
        .or(parsed.web)
        .filter(|c| !c.client_id.trim().is_empty() && !c.client_secret.trim().is_empty())
        .ok_or_else(|| refuse(NOT_GOOGLE_JSON))?;
    let secret = Secret::new(client.client_secret);
    Ok(ResolvedSecret {
        client_id: Some(client.client_id),
        tenant: None,
        login: Some(secret.clone()),
        stored: Some(secret),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http_client;
    use crate::ops::ErrorCode;
    use crate::test_support::FakeOp;

    const GOOGLE: &str = r#"{"installed":{"client_id":"123.apps.googleusercontent.com","project_id":"rail-test","auth_uri":"https://accounts.google.com/o/oauth2/auth","client_secret":"GOCSPX-from-file","redirect_uris":["http://localhost"]}}"#;

    fn op(tmp: &tempfile::TempDir) -> OpCli {
        OpCli::from_config(None, &tmp.path().join("run"))
    }

    async fn resolve(
        tmp: &tempfile::TempDir,
        input: ClientSecretInput,
    ) -> Result<ResolvedSecret, OpError> {
        resolve_client_secret(Some(&input), &http_client().unwrap(), &op(tmp)).await
    }

    #[tokio::test]
    async fn a_relative_client_file_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve(&tmp, ClientSecretInput::JsonFile("client.json".into()))
            .await
            .unwrap_err();
        assert!(err.message.contains("use an absolute path"), "{err}");
    }

    #[tokio::test]
    async fn a_google_client_file_gives_the_id_and_the_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("client_secret_123.json");
        std::fs::write(&file, GOOGLE).unwrap();
        let resolved = resolve(&tmp, ClientSecretInput::JsonFile(file))
            .await
            .unwrap();
        assert_eq!(
            resolved.client_id.as_deref(),
            Some("123.apps.googleusercontent.com")
        );
        assert_eq!(
            resolved.login.as_ref().unwrap().expose(),
            "GOCSPX-from-file"
        );
        assert_eq!(resolved.stored, resolved.login);
        assert!(!format!("{resolved:?}").contains("GOCSPX"));
    }

    /// Review focus 6: an agent can point this at any file; nothing of it may come back.
    #[tokio::test]
    async fn a_client_secret_file_that_is_not_google_json_never_echoes_its_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            (
                "ssh",
                "-----BEGIN OPENSSH PRIVATE KEY-----\nTOP-SECRET-KEY-MATERIAL\n",
            ),
            (
                "json",
                r#"{"aws_secret_access_key": "TOP-SECRET-KEY-MATERIAL"}"#,
            ),
            (
                "partial",
                r#"{"installed": {"client_id": "TOP-SECRET-KEY-MATERIAL"}}"#,
            ),
        ];
        for (name, body) in cases {
            let file = tmp.path().join(name);
            std::fs::write(&file, body).unwrap();
            let err = resolve(&tmp, ClientSecretInput::JsonFile(file.clone()))
                .await
                .unwrap_err();
            let text = format!("{err} {err:?} {}", serde_json::to_string(&err).unwrap());
            assert!(!text.contains("TOP-SECRET-KEY-MATERIAL"), "{name}: {text}");
            assert!(
                err.message.contains("not a Google client secret JSON file"),
                "{err}"
            );
            assert!(err.message.contains(&file.display().to_string()), "{err}");
            assert_eq!(err.code, ErrorCode::InvalidInput);
        }
    }

    /// Review focus 6.
    #[tokio::test]
    async fn an_oversized_client_secret_file_is_refused_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("huge.json");
        let mut body = GOOGLE.to_string();
        body.push_str(&" ".repeat(MAX_CLIENT_FILE_BYTES as usize));
        std::fs::write(&file, body).unwrap();
        let err = resolve(&tmp, ClientSecretInput::JsonFile(file))
            .await
            .unwrap_err();
        assert!(err.message.contains("larger than 64 KiB"), "{err}");
        assert!(!err.message.contains("GOCSPX"), "{err}");
        let missing = resolve(&tmp, ClientSecretInput::JsonFile(tmp.path().join("nope")))
            .await
            .unwrap_err();
        assert!(missing.message.contains("cannot be read"), "{missing}");
    }

    /// Review focus 6: a device or a directory is refused without being opened for reading.
    #[tokio::test]
    async fn a_client_secret_path_that_is_not_a_regular_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for path in [tmp.path().to_path_buf(), "/dev/null".into()] {
            let err = resolve(&tmp, ClientSecretInput::JsonFile(path))
                .await
                .unwrap_err();
            assert!(err.message.contains("is not a regular file"), "{err}");
            assert_eq!(err.code, ErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn a_value_that_is_a_reference_is_handled_as_a_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeOp::install(tmp.path());
        fake.set_read("GOCSPX-from-1password");
        let input = ClientSecretInput::Value(Secret::new("op://Rail/google/client_secret"));
        let resolved = resolve_client_secret(Some(&input), &http_client().unwrap(), &fake.cli())
            .await
            .unwrap();
        assert_eq!(resolved.login.unwrap().expose(), "GOCSPX-from-1password");
        assert_eq!(
            resolved.stored.unwrap().expose(),
            "op://Rail/google/client_secret"
        );
        let bad = ClientSecretInput::Value(Secret::new("op://nonsense"));
        assert!(
            resolve_client_secret(Some(&bad), &http_client().unwrap(), &fake.cli())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_preset_may_not_carry_a_1password_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("evil.toml");
        std::fs::write(
            &file,
            "kind = \"gmail\"\nclient_id = \"x\"\nclient_secret = \"op://Rail/other/secret\"\n",
        )
        .unwrap();
        let err = resolve(&tmp, ClientSecretInput::Preset(file.display().to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Preset);
        assert_eq!(err.message, "a preset must not carry a 1Password reference");
    }

    /// Nothing that is not a regular file is opened: a FIFO opened here would block this test.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fifo_is_refused_without_being_opened() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("pipe.json");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            resolve(&tmp, ClientSecretInput::JsonFile(fifo.clone())),
        )
        .await
        .expect("a FIFO must be refused without blocking")
        .unwrap_err();
        assert!(
            refused.message.contains("is not a regular file"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_reference_is_read_with_op_and_stored_as_the_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeOp::install(tmp.path());
        fake.set_read("GOCSPX-from-1password");
        let input = ClientSecretInput::Reference("op://Rail/google/client_secret".into());
        let resolved = resolve_client_secret(Some(&input), &http_client().unwrap(), &fake.cli())
            .await
            .unwrap();
        assert_eq!(resolved.login.unwrap().expose(), "GOCSPX-from-1password");
        assert_eq!(
            resolved.stored.unwrap().expose(),
            "op://Rail/google/client_secret"
        );
        assert!(!fake.argv().contains("GOCSPX"));
    }

    #[tokio::test]
    async fn a_preset_file_gives_its_client_id_tenant_and_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("acme.toml");
        std::fs::write(
            &file,
            "kind = \"gmail\"\nclient_id = \"acme\"\nclient_secret = \"GOCSPX-acme\"\n",
        )
        .unwrap();
        let source = file.display().to_string();
        let resolved = resolve(&tmp, ClientSecretInput::Preset(source))
            .await
            .unwrap();
        assert_eq!(resolved.client_id.as_deref(), Some("acme"));
        assert_eq!(resolved.login.unwrap().expose(), "GOCSPX-acme");
        let plain = resolve(
            &tmp,
            ClientSecretInput::Preset("http://example.invalid/p.toml".into()),
        )
        .await
        .unwrap_err();
        assert_eq!(plain.code, ErrorCode::Preset);
    }

    /// Review focus 6: a preset path may be any file the user can read.
    #[tokio::test]
    async fn a_preset_path_that_is_not_a_preset_never_echoes_its_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("id_ed25519");
        std::fs::write(
            &file,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nTOP-SECRET-KEY-MATERIAL\n",
        )
        .unwrap();
        let source = file.display().to_string();
        let err = resolve(&tmp, ClientSecretInput::Preset(source.clone()))
            .await
            .unwrap_err();
        let text = format!("{err} {err:?} {}", serde_json::to_string(&err).unwrap());
        assert!(!text.contains("TOP-SECRET-KEY-MATERIAL"), "{text}");
        assert!(err.message.starts_with(&source), "{err}");
        assert!(err.message.contains("not a valid Rail preset"), "{err}");
        let big = tmp.path().join("big.toml");
        std::fs::write(&big, "#".repeat(70 * 1024)).unwrap();
        let err = resolve(&tmp, ClientSecretInput::Preset(big.display().to_string()))
            .await
            .unwrap_err();
        assert!(err.message.contains("larger than 64 KiB"), "{err}");
    }

    #[tokio::test]
    async fn a_value_is_used_as_it_is_and_nothing_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve(&tmp, ClientSecretInput::Value(Secret::new("sec")))
            .await
            .unwrap();
        assert_eq!(resolved.login.unwrap().expose(), "sec");
        let none = resolve_client_secret(None, &http_client().unwrap(), &op(&tmp))
            .await
            .unwrap();
        assert!(none.login.is_none() && none.stored.is_none() && none.client_id.is_none());
    }
}
