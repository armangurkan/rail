use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::config::SecretsBackend;
use crate::error::ErrorClass;
use crate::paths::{Paths, write_private_atomic};

const KEYRING_SERVICE: &str = "rail";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// Hides both tokens so that `{:?}` and tracing fields never leak credentials.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error(
        "keyring unavailable: {0}. Set `secrets = \"file\"` under [general] in config.toml to keep tokens in a private file instead"
    )]
    KeyringUnavailable(String),
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("secrets file {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("secrets file {} is corrupt: {message}", .path.display())]
    Corrupt { path: PathBuf, message: String },
    #[error("token data is corrupt: {0}")]
    Encoding(String),
}

impl SecretError {
    /// Filesystem and keyring hiccups may clear up; a missing keyring or a corrupt store will not.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Io { .. } | Self::Keyring(_) => ErrorClass::Transient,
            Self::KeyringUnavailable(_) | Self::Corrupt { .. } | Self::Encoding(_) => {
                ErrorClass::Permanent
            }
        }
    }
}

pub trait SecretStore: Send + Sync {
    fn get_tokens(&self, address: &str) -> Result<Option<TokenSet>, SecretError>;
    fn set_tokens(&self, address: &str, tokens: &TokenSet) -> Result<(), SecretError>;
    fn delete_tokens(&self, address: &str) -> Result<(), SecretError>;
    fn backend_name(&self) -> &'static str;
}

pub fn open_secret_store(backend: SecretsBackend, paths: &Paths) -> Arc<dyn SecretStore> {
    match backend {
        SecretsBackend::Keyring => Arc::new(KeyringSecretStore),
        SecretsBackend::File => Arc::new(FileSecretStore::new(paths.secrets_file())),
    }
}

/// Describe a JSON error by kind and position only. serde_json's own text
/// quotes the offending value, which here may be a token.
fn json_message(e: &serde_json::Error) -> String {
    let kind = match e.classify() {
        serde_json::error::Category::Io => "I/O error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "unexpected value",
        serde_json::error::Category::Eof => "unexpected end of data",
    };
    format!("{kind} at line {} column {}", e.line(), e.column())
}

fn encode(tokens: &TokenSet) -> Result<String, SecretError> {
    serde_json::to_string(tokens).map_err(|e| SecretError::Encoding(json_message(&e)))
}

fn decode(json: &str) -> Result<TokenSet, SecretError> {
    serde_json::from_str(json).map_err(|e| SecretError::Encoding(json_message(&e)))
}

// ---- OS keyring ----

pub struct KeyringSecretStore;

fn map_keyring(e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoStorageAccess(inner) | keyring::Error::PlatformFailure(inner) => {
            SecretError::KeyringUnavailable(inner.to_string())
        }
        keyring::Error::NoDefaultStore => {
            SecretError::KeyringUnavailable("no credential store on this system".to_string())
        }
        other => SecretError::Keyring(other.to_string()),
    }
}

fn entry(address: &str) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(KEYRING_SERVICE, address).map_err(map_keyring)
}

impl SecretStore for KeyringSecretStore {
    fn get_tokens(&self, address: &str) -> Result<Option<TokenSet>, SecretError> {
        match entry(address)?.get_password() {
            Ok(json) => decode(&json).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring(e)),
        }
    }

    fn set_tokens(&self, address: &str, tokens: &TokenSet) -> Result<(), SecretError> {
        entry(address)?
            .set_password(&encode(tokens)?)
            .map_err(map_keyring)
    }

    fn delete_tokens(&self, address: &str) -> Result<(), SecretError> {
        match entry(address)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring(e)),
        }
    }

    fn backend_name(&self) -> &'static str {
        "keyring"
    }
}

// ---- 0600 JSON file ----

pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn io_err(&self, source: std::io::Error) -> SecretError {
        SecretError::Io {
            path: self.path.clone(),
            source,
        }
    }

    fn read_all(&self) -> Result<HashMap<String, TokenSet>, SecretError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| SecretError::Corrupt {
                path: self.path.clone(),
                message: json_message(&e),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(self.io_err(e)),
        }
    }

    fn write_all(&self, map: &HashMap<String, TokenSet>) -> Result<(), SecretError> {
        let json =
            serde_json::to_vec_pretty(map).map_err(|e| SecretError::Encoding(json_message(&e)))?;
        write_private_atomic(&self.path, &json).map_err(|e| self.io_err(e))
    }
}

impl SecretStore for FileSecretStore {
    fn get_tokens(&self, address: &str) -> Result<Option<TokenSet>, SecretError> {
        Ok(self.read_all()?.remove(address))
    }

    fn set_tokens(&self, address: &str, tokens: &TokenSet) -> Result<(), SecretError> {
        let mut map = self.read_all()?;
        map.insert(address.to_string(), tokens.clone());
        self.write_all(&map)
    }

    fn delete_tokens(&self, address: &str) -> Result<(), SecretError> {
        let mut map = self.read_all()?;
        if map.remove(address).is_some() {
            self.write_all(&map)?;
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "file"
    }
}

// ---- in-memory, for tests ----

#[derive(Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<String, TokenSet>>,
}

impl SecretStore for MemorySecretStore {
    fn get_tokens(&self, address: &str) -> Result<Option<TokenSet>, SecretError> {
        Ok(self
            .inner
            .lock()
            .expect("secrets mutex")
            .get(address)
            .cloned())
    }

    fn set_tokens(&self, address: &str, tokens: &TokenSet) -> Result<(), SecretError> {
        self.inner
            .lock()
            .expect("secrets mutex")
            .insert(address.to_string(), tokens.clone());
        Ok(())
    }

    fn delete_tokens(&self, address: &str) -> Result<(), SecretError> {
        self.inner.lock().expect("secrets mutex").remove(address);
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "acc".to_string(),
            refresh_token: "ref".to_string(),
            expires_at: 1_700_000_000,
        }
    }

    #[test]
    fn memory_store_roundtrips() {
        let store = MemorySecretStore::default();
        assert_eq!(store.get_tokens("a@x.com").unwrap(), None);
        store.set_tokens("a@x.com", &tokens()).unwrap();
        assert_eq!(store.get_tokens("a@x.com").unwrap(), Some(tokens()));
        store.delete_tokens("a@x.com").unwrap();
        assert_eq!(store.get_tokens("a@x.com").unwrap(), None);
    }

    #[test]
    fn file_store_roundtrips_and_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("secrets.json");
        let store = FileSecretStore::new(path.clone());
        assert_eq!(store.get_tokens("a@x.com").unwrap(), None);
        store.set_tokens("a@x.com", &tokens()).unwrap();
        store.set_tokens("b@x.com", &tokens()).unwrap();
        assert_eq!(
            FileSecretStore::new(path.clone())
                .get_tokens("a@x.com")
                .unwrap(),
            Some(tokens())
        );
        store.delete_tokens("a@x.com").unwrap();
        assert_eq!(store.get_tokens("a@x.com").unwrap(), None);
        assert_eq!(store.get_tokens("b@x.com").unwrap(), Some(tokens()));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        FileSecretStore::new(path.clone())
            .set_tokens("a@x.com", &tokens())
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_creates_its_directory_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        FileSecretStore::new(data_dir.join("secrets.json"))
            .set_tokens("a@x.com", &tokens())
            .unwrap();
        let mode = std::fs::metadata(&data_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn corrupt_file_is_reported_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = FileSecretStore::new(path.clone());
        let err = store.get_tokens("a@x.com").unwrap_err();
        assert!(matches!(err, SecretError::Corrupt { .. }), "{err}");
        assert!(store.set_tokens("a@x.com", &tokens()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn corrupt_value_errors_never_echo_the_value() {
        const TOKEN: &str = "ya29.super-secret-access-token";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, format!("{{\"a@x.com\": \"{TOKEN}\"}}")).unwrap();
        let err = FileSecretStore::new(path)
            .get_tokens("a@x.com")
            .unwrap_err();
        assert!(matches!(err, SecretError::Corrupt { .. }), "{err}");
        assert!(!err.to_string().contains(TOKEN), "{err}");
        assert!(!format!("{err:?}").contains(TOKEN), "{err:?}");
        assert!(err.to_string().contains("line 1"), "{err}");

        let err = decode(&format!("\"{TOKEN}\"")).unwrap_err();
        assert!(matches!(err, SecretError::Encoding(_)), "{err}");
        assert!(!err.to_string().contains(TOKEN), "{err}");
        assert!(!format!("{err:?}").contains(TOKEN), "{err:?}");
    }

    #[test]
    fn keyring_unavailable_error_names_file_fallback() {
        let err = map_keyring(keyring::Error::NoStorageAccess("no dbus".into()));
        assert!(matches!(err, SecretError::KeyringUnavailable(_)));
        assert!(err.to_string().contains("secrets = \"file\""), "{err}");
        let err = map_keyring(keyring::Error::NoDefaultStore);
        assert!(matches!(err, SecretError::KeyringUnavailable(_)));
    }

    #[test]
    fn open_secret_store_picks_backend() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::all_in(dir.path());
        assert_eq!(
            open_secret_store(SecretsBackend::File, &paths).backend_name(),
            "file"
        );
        assert_eq!(
            open_secret_store(SecretsBackend::Keyring, &paths).backend_name(),
            "keyring"
        );
    }

    #[test]
    fn debug_redacts_tokens() {
        let output = format!("{:?}", tokens());
        assert!(output.contains("<redacted>"), "{output}");
        assert!(output.contains("1700000000"), "{output}");
        assert!(!output.contains("\"acc\""), "{output}");
        assert!(!output.contains("\"ref\""), "{output}");
    }

    #[test]
    fn class_splits_transient_from_permanent() {
        let io = SecretError::Io {
            path: PathBuf::from("secrets.json"),
            source: std::io::Error::other("disk"),
        };
        assert_eq!(io.class(), ErrorClass::Transient);
        assert_eq!(
            SecretError::Keyring("boom".into()).class(),
            ErrorClass::Transient
        );
        assert_eq!(
            SecretError::KeyringUnavailable("no dbus".into()).class(),
            ErrorClass::Permanent
        );
        let corrupt = SecretError::Corrupt {
            path: PathBuf::from("secrets.json"),
            message: "eof".into(),
        };
        assert_eq!(corrupt.class(), ErrorClass::Permanent);
        assert_eq!(
            SecretError::Encoding("bad".into()).class(),
            ErrorClass::Permanent
        );
    }

    /// Talks to the real OS keyring. Run with `cargo test -- --ignored keyring_live`.
    #[test]
    #[ignore]
    fn keyring_live_roundtrip() {
        let store = KeyringSecretStore;
        let address = "rail-test@example.invalid";
        store.set_tokens(address, &tokens()).unwrap();
        assert_eq!(store.get_tokens(address).unwrap(), Some(tokens()));
        store.delete_tokens(address).unwrap();
        assert_eq!(store.get_tokens(address).unwrap(), None);
    }
}
