//! The 1Password backend: one item per mail account in a vault, titled `Rail: <address>` and
//! tagged `rail`, through the `op` CLI. Values reach `op` only inside a JSON template on stdin,
//! never as command-line arguments, which other processes can read. Every call runs with a
//! timeout long enough for an unlock prompt.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::secrets::{SecretError, SecretStore, TokenSet, json_message};
use crate::config::OnePasswordConfig;
use crate::ops::{OP_REFERENCE_PREFIX, Secret};
use crate::paths::create_private_dir;

/// How long one `op` call may take, unlock prompt included.
pub const OP_TIMEOUT: Duration = Duration::from_secs(30);
const TAG: &str = "rail";
const TOKEN_FIELDS: &str = "label=access_token,label=refresh_token,label=expires_at";
const POLL: Duration = Duration::from_millis(20);
const LOCKED_MARKERS: [&str; 4] = [
    "not currently signed in",
    "not signed in",
    "op signin",
    "authorization prompt dismissed",
];
const NO_ITEM_MARKER: &str = "isn't an item";
const MAX_ERROR_CHARS: usize = 300;
/// `ETXTBSY`: a just-written executable is briefly busy while another thread forks.
const TEXT_FILE_BUSY: i32 = 26;
const SPAWN_ATTEMPTS: usize = 5;

/// How to run `op`: which binary, which account, how long to wait.
#[derive(Debug, Clone)]
pub struct OpCli {
    program: PathBuf,
    account: Option<String>,
    timeout: Duration,
    /// Why a configured `op_path` may not be run; every call then fails with it.
    refused: Option<String>,
}

impl OpCli {
    /// `op` on PATH unless `[onepassword] op_path` says otherwise. A configured `op_path` is
    /// checked now (see `check_op_path`); the current user is the owner of `runtime_dir`, which
    /// is created owner-only if it does not exist.
    pub fn from_config(config: Option<&OnePasswordConfig>, runtime_dir: &Path) -> Self {
        let configured = config.and_then(|c| c.op_path.clone());
        let refused = configured.as_deref().and_then(|path| {
            own_uid(runtime_dir)
                .and_then(|uid| check_op_path(path, uid))
                .err()
        });
        Self {
            program: configured.unwrap_or_else(|| PathBuf::from("op")),
            account: config.and_then(|c| c.account.clone()),
            timeout: OP_TIMEOUT,
            refused,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run `op` with `args` (never a secret) and optional `stdin`, and return its stdout.
    /// Every string in `redact` is removed from the error text before it leaves this function.
    pub fn run(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        redact: &[&str],
    ) -> Result<Vec<u8>, SecretError> {
        let mut command = Command::new(&self.program);
        command.args(args);
        if let Some(account) = &self.account {
            command.args(["--account", account.as_str()]);
        }
        let input = if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        command
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn(&mut command)?;
        if let Err(e) = feed(&mut child, stdin)
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            // A broken pipe means op already exited; its status and stderr say why.
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!(kind = ?e.kind(), "could not write to op");
            return Err(SecretError::OnePassword(format!(
                "could not write to op ({:?})",
                e.kind()
            )));
        }
        let (status, stdout, stderr) = wait_with_timeout(child, self.timeout)?;
        if status.success() {
            return Ok(stdout);
        }
        Err(classify(&stderr, redact))
    }

    fn spawn(&self, command: &mut Command) -> Result<Child, SecretError> {
        if let Some(reason) = &self.refused {
            return Err(SecretError::OnePasswordUnsafe {
                path: self.program.display().to_string(),
                reason: reason.clone(),
            });
        }
        let mut attempt = 1;
        loop {
            match command.spawn() {
                Err(e) if e.raw_os_error() == Some(TEXT_FILE_BUSY) && attempt < SPAWN_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(POLL);
                }
                result => {
                    return result.map_err(|e| SecretError::OnePasswordMissing {
                        program: self.program.display().to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    /// Whether `op` is installed, unlocked and signed in.
    pub fn whoami(&self) -> Result<(), SecretError> {
        self.run(&["whoami", "--format", "json"], None, &[])
            .map(drop)
    }

    /// The value behind an `op://vault/item/field` reference. Never cached or logged.
    pub fn read_reference(&self, reference: &str) -> Result<Secret, SecretError> {
        validate_reference(reference)?;
        let out = self.run(&["read", "--no-newline", reference], None, &[])?;
        let text = String::from_utf8(out)
            .map_err(|_| SecretError::OnePassword(format!("{reference} is not text")))?;
        Ok(Secret::new(text.trim_end_matches(['\r', '\n'])))
    }
}

/// The current user's uid, read without `unsafe` as the owner of a directory Rail creates.
fn own_uid(runtime_dir: &Path) -> Result<u32, String> {
    create_private_dir(runtime_dir)
        .and_then(|()| std::fs::metadata(runtime_dir))
        .map(|m| m.uid())
        .map_err(|e| format!("cannot tell who the current user is ({e})"))
}

/// A configured `op` binary must be an absolute path to a regular, executable file owned by
/// root or by `own_uid`, writable by nobody else. Anything less would let another user, or
/// anything that can write that file, run code with the tokens on its stdin.
pub fn check_op_path(path: &Path, own_uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("op_path must be an absolute path".to_string());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("it cannot be read ({e})"))?;
    if !meta.is_file() {
        return Err("it is not a regular file".to_string());
    }
    let mode = meta.permissions().mode();
    if mode & 0o111 == 0 {
        return Err("it is not executable".to_string());
    }
    if mode & 0o022 != 0 {
        return Err("it is writable by group or others".to_string());
    }
    if meta.uid() != 0 && meta.uid() != own_uid {
        return Err("it is owned by another user".to_string());
    }
    Ok(())
}

/// `op://vault/item/field`, every part present.
pub fn validate_reference(reference: &str) -> Result<(), SecretError> {
    let parts: Vec<&str> = reference
        .strip_prefix(OP_REFERENCE_PREFIX)
        .map(|rest| rest.split('/').collect())
        .unwrap_or_default();
    if parts.len() < 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(SecretError::OnePassword(format!(
            "{reference:?} is not a secret reference of the form op://vault/item/field"
        )));
    }
    Ok(())
}

/// Write the template to `op`'s stdin and close it, so `op` sees the end of its input.
fn feed(child: &mut Child, stdin: Option<&[u8]>) -> std::io::Result<()> {
    match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => pipe.write_all(bytes),
        _ => Ok(()),
    }
}

/// Wait for `child`, killing it after `timeout`. Its output is drained on threads so a full
/// pipe can never stall it.
fn wait_with_timeout(
    mut child: Child,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SecretError> {
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SecretError::OnePasswordTimeout);
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(SecretError::OnePassword(format!("waiting for op: {e}"))),
        }
    };
    Ok((status, joined(stdout)?, joined(stderr)?))
}

type Drain = JoinHandle<std::io::Result<Vec<u8>>>;

fn drain(mut pipe: impl Read + Send + 'static) -> Drain {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.read_to_end(&mut buffer).map(|_| buffer)
    })
}

/// The drained bytes. A read error or a panicked reader is logged (never with content) and
/// reported, not swallowed: output that could not be read must not pass for empty output.
fn joined(handle: Option<Drain>) -> Result<Vec<u8>, SecretError> {
    let Some(handle) = handle else {
        return Ok(Vec::new());
    };
    match handle.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => {
            tracing::warn!(kind = ?e.kind(), "could not read op's output");
            Err(SecretError::OnePassword(format!(
                "could not read op's output ({:?})",
                e.kind()
            )))
        }
        Err(_) => {
            tracing::warn!("the thread reading op's output panicked");
            Err(SecretError::OnePassword(
                "could not read op's output".to_string(),
            ))
        }
    }
}

/// Turn `op`'s stderr into an error: its last line, without the log prefix, with every
/// redacted value removed, cut to a sane length.
fn classify(stderr: &[u8], redact: &[&str]) -> SecretError {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("op failed without a message");
    let mut clean = strip_log_prefix(line);
    for value in redact.iter().filter(|v| !v.is_empty()) {
        clean = clean.replace(value, "<redacted>");
        // The template quotes the value as JSON, so an escaped form can appear too.
        if let Some(escaped) = json_escaped(value) {
            clean = clean.replace(&escaped, "<redacted>");
        }
    }
    let clean: String = clean.chars().take(MAX_ERROR_CHARS).collect();
    let lower = clean.to_lowercase();
    if LOCKED_MARKERS.iter().any(|m| lower.contains(m)) {
        SecretError::OnePasswordLocked(clean)
    } else if lower.contains(NO_ITEM_MARKER) {
        SecretError::OnePasswordNoItem(clean)
    } else {
        SecretError::OnePassword(clean)
    }
}

/// `value` as it appears inside a JSON string, without the quotes.
fn json_escaped(value: &str) -> Option<String> {
    let quoted = serde_json::to_string(value).ok()?;
    let inner = quoted.get(1..quoted.len().checked_sub(1)?)?;
    (inner != value).then(|| inner.to_string())
}

/// `[ERROR] 2026/10/10 08:35:58 You are not signed in` -> `You are not signed in`.
fn strip_log_prefix(line: &str) -> String {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("[ERROR]") else {
        return line.to_string();
    };
    let mut words = rest.trim_start().splitn(3, ' ');
    match (words.next(), words.next(), words.next()) {
        (Some(date), Some(time), Some(message)) if date.contains('/') && time.contains(':') => {
            message.to_string()
        }
        _ => rest.trim().to_string(),
    }
}

fn parse_json<T: DeserializeOwned>(out: &[u8]) -> Result<T, SecretError> {
    serde_json::from_slice(out).map_err(|e| SecretError::Encoding(json_message(&e)))
}

#[derive(Deserialize)]
struct ItemRef {
    id: String,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct FieldValue {
    label: String,
    #[serde(default)]
    value: String,
}

/// An item template. `fields` is last, so the JSON ends with the field array.
#[derive(Serialize)]
struct ItemTemplate<'a> {
    title: &'a str,
    category: &'static str,
    tags: [&'static str; 1],
    fields: Vec<TemplateField>,
}

#[derive(Serialize)]
struct TemplateField {
    id: &'static str,
    label: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    value: String,
}

fn concealed(id: &'static str, value: &str) -> TemplateField {
    TemplateField {
        id,
        label: id,
        kind: "CONCEALED",
        value: value.to_string(),
    }
}

fn template_json(template: &ItemTemplate<'_>) -> Result<Vec<u8>, SecretError> {
    serde_json::to_vec(template).map_err(|e| SecretError::Encoding(json_message(&e)))
}

fn token_template<'a>(title: &'a str, tokens: &TokenSet) -> ItemTemplate<'a> {
    ItemTemplate {
        title,
        category: "SECURE_NOTE",
        tags: [TAG],
        fields: vec![
            concealed("access_token", &tokens.access_token),
            concealed("refresh_token", &tokens.refresh_token),
            TemplateField {
                id: "expires_at",
                label: "expires_at",
                kind: "STRING",
                value: tokens.expires_at.to_string(),
            },
        ],
    }
}

fn tokens_from_fields(fields: &[FieldValue]) -> Result<TokenSet, SecretError> {
    let field = |label: &str| {
        fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.value.clone())
            .ok_or_else(|| SecretError::Encoding(format!("the 1Password item has no {label}")))
    };
    let expires_at = field("expires_at")?
        .trim()
        .parse()
        .map_err(|_| SecretError::Encoding("expires_at is not a number".to_string()))?;
    Ok(TokenSet {
        access_token: field("access_token")?,
        refresh_token: field("refresh_token")?,
        expires_at,
    })
}

/// Item ids by vault and title, so most calls skip the `op item list` lookup.
#[derive(Debug, Clone, Default)]
pub struct ItemIdCache(Arc<Mutex<HashMap<String, String>>>);

impl ItemIdCache {
    fn key(vault: &str, title: &str) -> String {
        format!("{vault}/{title}")
    }

    fn get(&self, vault: &str, title: &str) -> Option<String> {
        let ids = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ids.get(&Self::key(vault, title)).cloned()
    }

    fn put(&self, vault: &str, title: &str, id: &str) {
        let mut ids = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ids.insert(Self::key(vault, title), id.to_string());
    }

    fn forget(&self, vault: &str, title: &str) {
        let mut ids = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ids.remove(&Self::key(vault, title));
    }
}

pub struct OnePasswordSecretStore {
    op: OpCli,
    vault: String,
    ids: ItemIdCache,
}

fn item_title(address: &str) -> String {
    format!("Rail: {}", address.to_ascii_lowercase())
}

impl OnePasswordSecretStore {
    pub fn new(op: OpCli, vault: String, ids: ItemIdCache) -> Self {
        Self { op, vault, ids }
    }

    fn vault(&self) -> Result<&str, SecretError> {
        if self.vault.trim().is_empty() {
            return Err(SecretError::OnePassword(
                "no vault is set; add vault = \"...\" under [onepassword]".to_string(),
            ));
        }
        Ok(&self.vault)
    }

    fn find(&self, title: &str) -> Result<Option<String>, SecretError> {
        let vault = self.vault()?;
        if let Some(id) = self.ids.get(vault, title) {
            return Ok(Some(id));
        }
        let args = [
            "item", "list", "--vault", vault, "--tags", TAG, "--format", "json",
        ];
        let out = self.op.run(&args, None, &[])?;
        let items: Vec<ItemRef> = if out.iter().all(u8::is_ascii_whitespace) {
            Vec::new()
        } else {
            parse_json(&out)?
        };
        let id = items.into_iter().find(|i| i.title == title).map(|i| i.id);
        if let Some(id) = &id {
            self.ids.put(vault, title, id);
        }
        Ok(id)
    }

    fn create(&self, title: &str, json: &[u8], redact: &[&str]) -> Result<(), SecretError> {
        let vault = self.vault()?;
        let args = ["item", "create", "--vault", vault, "--format", "json", "-"];
        let created: ItemRef = parse_json(&self.op.run(&args, Some(json), redact)?)?;
        self.ids.put(vault, title, &created.id);
        Ok(())
    }
}

impl SecretStore for OnePasswordSecretStore {
    fn get_tokens(&self, address: &str) -> Result<Option<TokenSet>, SecretError> {
        let title = item_title(address);
        let Some(id) = self.find(&title)? else {
            return Ok(None);
        };
        let vault = self.vault()?;
        let args = [
            "item",
            "get",
            &id,
            "--vault",
            vault,
            "--fields",
            TOKEN_FIELDS,
            "--reveal",
            "--format",
            "json",
        ];
        match self.op.run(&args, None, &[]) {
            Ok(out) => tokens_from_fields(&parse_json::<Vec<FieldValue>>(&out)?).map(Some),
            Err(SecretError::OnePasswordNoItem(_)) => {
                self.ids.forget(vault, &title);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn set_tokens(&self, address: &str, tokens: &TokenSet) -> Result<(), SecretError> {
        let title = item_title(address);
        let json = template_json(&token_template(&title, tokens))?;
        let redact = [tokens.access_token.as_str(), tokens.refresh_token.as_str()];
        let Some(id) = self.find(&title)? else {
            return self.create(&title, &json, &redact);
        };
        let vault = self.vault()?;
        let args = ["item", "edit", &id, "--vault", vault, "--format", "json"];
        match self.op.run(&args, Some(&json), &redact) {
            Ok(_) => Ok(()),
            Err(SecretError::OnePasswordNoItem(_)) => {
                self.ids.forget(vault, &title);
                self.create(&title, &json, &redact)
            }
            Err(e) => Err(e),
        }
    }

    fn delete_tokens(&self, address: &str) -> Result<(), SecretError> {
        let title = item_title(address);
        let Some(id) = self.find(&title)? else {
            return Ok(());
        };
        let vault = self.vault()?;
        match self
            .op
            .run(&["item", "delete", &id, "--vault", vault], None, &[])
        {
            Ok(_) | Err(SecretError::OnePasswordNoItem(_)) => {
                self.ids.forget(vault, &title);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn backend_name(&self) -> &'static str {
        "1password"
    }

    fn probe(&self) -> Result<(), SecretError> {
        self.vault()?;
        self.op.whoami()
    }
}

/// Put a client secret in 1Password and return the `op://` reference to write into config.
pub fn store_client_secret(
    op: &OpCli,
    vault: &str,
    client_id: &str,
    secret: &Secret,
) -> Result<String, SecretError> {
    let title = format!("Rail client: {client_id}");
    let template = ItemTemplate {
        title: &title,
        category: "SECURE_NOTE",
        tags: [TAG],
        fields: vec![concealed("client_secret", secret.expose())],
    };
    let args = ["item", "create", "--vault", vault, "--format", "json", "-"];
    let out = op.run(&args, Some(&template_json(&template)?), &[secret.expose()])?;
    let created: ItemRef = parse_json(&out)?;
    Ok(format!(
        "{OP_REFERENCE_PREFIX}{vault}/{}/client_secret",
        created.id
    ))
}

#[cfg(test)]
mod tests;
