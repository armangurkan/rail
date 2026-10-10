//! Runs the wizard's `Action`s with the code the flag commands already use, and reports
//! each outcome back as a wizard `Event`. Synchronous file and keyring work runs on the
//! blocking pool, so the event loop never waits on I/O.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rail_core::accounts;
use rail_core::auth::{ItemIdCache, TokenSet, http_client, run_admin_consent};
use rail_core::config::AccountConfig;
use rail_core::error::ErrorClass;
use rail_core::onboarding::{
    Action, DoneReport, Event, Failure, PresetEntry, discover_presets, fetch_preset, read_preset,
};
use rail_core::ops::{AccountAddRequest, ClientSecretInput, ErrorCode, OpError};
use rail_core::paths::Paths;
use url::Url;

use crate::browser;
use crate::test_hooks;
use crate::tui::app::{Effect, TaskResult, TaskSink};

/// Starts the browser on a URL. Injected so tests never start a real one.
pub type Opener = Arc<dyn Fn(&Url) -> io::Result<()> + Send + Sync>;

/// What the effects need from the outside world: where rail keeps its files, and how to
/// open a URL in the browser.
#[derive(Clone)]
pub struct EffectContext {
    pub paths: Paths,
    pub opener: Opener,
}

impl EffectContext {
    /// The real context: the user's paths and the platform browser.
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            opener: Arc::new(browser::launch),
        }
    }
}

/// The shell effect that carries out `action`. Work that reports back becomes a spawned
/// job sending `TaskResult::Wizard` events.
pub fn to_effect(action: Action, ctx: &EffectContext) -> Effect {
    let ctx = ctx.clone();
    match action {
        Action::Quit => Effect::Quit,
        Action::CancelPending => Effect::CancelAll,
        Action::Copy(text) => Effect::Copy(text),
        Action::OpenUrl(url) => job(move |_| async move { open(&ctx.opener, &url) }),
        Action::LoadPresets => job(move |sink| load_presets(ctx.paths, sink)),
        Action::ReadPreset(path) => job(move |sink| read_preset_file(path, sink)),
        Action::FetchPreset(url) => job(move |sink| fetch(url, sink)),
        Action::RunAdminConsent { client_id, tenant } => {
            job(move |sink| admin_consent(ctx.opener, client_id, tenant, sink))
        }
        Action::RunLogin(account) => job(move |sink| login(ctx, account, sink)),
        Action::Persist { account, tokens } => {
            job(move |sink| persist(ctx.paths, account, tokens, sink))
        }
    }
}

fn job<F, Fut>(run: F) -> Effect
where
    F: FnOnce(TaskSink) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Effect::Spawn(Box::new(move |sink| Box::pin(run(sink))))
}

fn send(sink: &TaskSink, event: Event) {
    // A closed channel means the event loop has ended and nobody is listening any more.
    let _ = sink.send(TaskResult::Wizard(event));
}

/// The URL is on screen whatever happens here, so a failed opener is logged, not fatal.
fn open(opener: &Opener, url: &Url) {
    if let Err(e) = opener(url) {
        tracing::warn!(error = %e, "could not start the browser opener");
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Failure::new(format!("internal error: {e}"), ErrorClass::Permanent))
}

async fn load_presets(paths: Paths, sink: TaskSink) {
    let entries = match blocking(move || discover_presets(&paths)).await {
        Ok(found) => found
            .into_iter()
            .map(|(source, preset)| PresetEntry {
                source,
                preset: preset.map_err(|e| e.to_string()),
            })
            .collect(),
        Err(failure) => vec![PresetEntry {
            source: PathBuf::new(),
            preset: Err(failure.message),
        }],
    };
    send(&sink, Event::PresetsLoaded(entries));
}

async fn read_preset_file(path: PathBuf, sink: TaskSink) {
    let result = blocking(move || read_preset(&path))
        .await
        .and_then(|read| read.map_err(|e| Failure::new(e.to_string(), e.class())));
    send(&sink, Event::PresetFetched(result));
}

async fn fetch(url: String, sink: TaskSink) {
    let result = match http_client() {
        Ok(http) => fetch_preset(&http, &url)
            .await
            .map_err(|e| Failure::new(e.to_string(), e.class())),
        Err(e) => Err(Failure::from_auth(&e)),
    };
    send(&sink, Event::PresetFetched(result));
}

/// Every `AuthError` becomes a `Failure` unchanged: the wizard recognises some by their text.
async fn admin_consent(opener: Opener, client_id: String, tenant: String, sink: TaskSink) {
    let result = run_admin_consent(&client_id, &tenant, |url| {
        send(&sink, Event::ConsentUrl(url.clone()));
        open(&opener, url);
    })
    .await;
    let result = result.map_err(|e| Failure::from_auth(&e));
    send(&sink, Event::AdminConsentFinished(result));
}

/// The flag form's order: every offline check, then the browser.
async fn login(ctx: EffectContext, account: AccountConfig, sink: TaskSink) {
    let result = login_with(&ctx, &account, &sink).await;
    send(&sink, Event::LoginFinished(result));
}

async fn login_with(
    ctx: &EffectContext,
    account: &AccountConfig,
    sink: &TaskSink,
) -> Result<TokenSet, Failure> {
    let http = http_client().map_err(|e| Failure::from_auth(&e))?;
    let ids = ItemIdCache::default();
    let prepared = accounts::prepare_and_probe(&ctx.paths, &ids, &add_request(account), &http)
        .await
        .map_err(|e| failure(&e))?;
    let token_url = test_hooks::token_url();
    accounts::run_login(
        &prepared.login_account(),
        token_url.as_deref(),
        &http,
        |url| {
            send(sink, Event::LoginUrl(url.clone()));
            open(&ctx.opener, url);
        },
    )
    .await
    .map_err(|e| Failure::from_auth(&e))
}

/// The account the wizard built, as an add request. A client secret typed as `op://...` is a
/// 1Password reference.
fn add_request(account: &AccountConfig) -> AccountAddRequest {
    AccountAddRequest {
        kind: account.kind,
        address: account.address.clone(),
        client_id: Some(account.client_id.clone()),
        client_secret: account
            .client_secret
            .clone()
            .map(ClientSecretInput::from_flag),
        tenant: account.tenant.clone(),
        name: Some(account.name.clone()),
        color: account.color.clone(),
        store_secret_in_1password: false,
        open_browser: true,
    }
}

async fn persist(paths: Paths, account: AccountConfig, tokens: TokenSet, sink: TaskSink) {
    let result = save_new_account(paths, account, tokens).await;
    send(&sink, Event::Persisted(result.map_err(|e| failure(&e))));
}

/// The flag form's persist: tokens, then the store row, then config.toml last. The checks are
/// repeated first (no probe: the login already used the store).
async fn save_new_account(
    paths: Paths,
    account: AccountConfig,
    tokens: TokenSet,
) -> Result<DoneReport, OpError> {
    let http = http_client()?;
    let prepared = accounts::prepare_request(&paths, &add_request(&account), &http).await?;
    tokio::task::spawn_blocking(move || {
        let saved = accounts::persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens)?;
        Ok(DoneReport {
            config_path: saved.config_path,
            token_backend: saved.token_backend,
            token_location: saved.token_location,
            store_path: saved.store_path,
        })
    })
    .await
    .map_err(|e| OpError::internal("a blocking task failed").with_detail(e.to_string()))?
}

/// An operation error as the wizard shows it: the message, detail and hint, its class, and
/// whether Microsoft wants an administrator's consent.
fn failure(error: &OpError) -> Failure {
    Failure {
        message: error.text(),
        class: error.class,
        consent_required: error.code == ErrorCode::ConsentRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use rail_core::auth::{FileSecretStore, SecretStore};
    use rail_core::config::AccountKind;
    use rail_core::store::Store;
    use tokio::sync::mpsc;

    const FILE_BACKEND: &str = "[general]\nsecrets = \"file\"\n";

    fn tmp_paths(tmp: &tempfile::TempDir, config: &str) -> Paths {
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        paths
    }

    /// An opener that records the URLs it is asked to open.
    fn recording_opener() -> (Opener, Arc<Mutex<Vec<Url>>>) {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let record = opened.clone();
        let opener: Opener = Arc::new(move |url: &Url| {
            record.lock().unwrap().push(url.clone());
            Ok(())
        });
        (opener, opened)
    }

    fn ctx(paths: Paths) -> EffectContext {
        EffectContext {
            paths,
            opener: recording_opener().0,
        }
    }

    async fn run(effect: Effect) -> Vec<Event> {
        let Effect::Spawn(job) = effect else {
            panic!("expected a spawned job, got {effect:?}");
        };
        let (tx, mut results) = mpsc::unbounded_channel();
        job(TaskSink::new(tx, 0)).await;
        let mut events = Vec::new();
        while let Ok((_, TaskResult::Wizard(event))) = results.try_recv() {
            events.push(event);
        }
        events
    }

    fn account(address: &str) -> AccountConfig {
        AccountConfig {
            name: "me".into(),
            kind: AccountKind::Gmail,
            address: address.into(),
            color: None,
            client_id: "cid".into(),
            client_secret: Some("sec".into()),
            tenant: None,
            signature: None,
            ai_send: None,
        }
    }

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1,
        }
    }

    #[test]
    fn plain_actions_map_to_shell_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(Paths::all_in(tmp.path()));
        assert!(matches!(to_effect(Action::Quit, &ctx), Effect::Quit));
        assert!(matches!(
            to_effect(Action::CancelPending, &ctx),
            Effect::CancelAll
        ));
        assert!(matches!(to_effect(Action::Copy("u".into()), &ctx), Effect::Copy(t) if t == "u"));
    }

    #[tokio::test]
    async fn load_presets_reports_files_from_the_config_presets_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = tmp_paths(&tmp, FILE_BACKEND);
        let dir = paths.config_dir.join("presets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("acme.toml"),
            "name = \"Acme\"\nkind = \"gmail\"\nclient_id = \"a\"\nclient_secret = \"s\"\n",
        )
        .unwrap();
        let events = run(to_effect(Action::LoadPresets, &ctx(paths))).await;
        let [Event::PresetsLoaded(entries)] = events.as_slice() else {
            panic!("{events:?}");
        };
        let acme = entries
            .iter()
            .find(|e| e.source.ends_with("acme.toml"))
            .unwrap();
        assert_eq!(acme.preset.as_ref().unwrap().label(), "Acme");
    }

    #[tokio::test]
    async fn read_preset_reports_a_parse_error_with_its_line() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = tmp_paths(&tmp, FILE_BACKEND);
        let file = tmp.path().join("bad.toml");
        std::fs::write(&file, "kind = \"gmail\"\nnope = 1\n").unwrap();
        let events = run(to_effect(Action::ReadPreset(file), &ctx(paths))).await;
        let [Event::PresetFetched(Err(failure))] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert!(
            failure.message.contains("bad.toml:2:"),
            "{}",
            failure.message
        );
    }

    #[tokio::test]
    async fn fetch_effect_refuses_plain_http() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = tmp_paths(&tmp, FILE_BACKEND);
        let action = Action::FetchPreset("http://127.0.0.1:9/preset.toml".into());
        let events = run(to_effect(action, &ctx(paths))).await;
        let [Event::PresetFetched(Err(failure))] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert!(
            failure.message.contains("must be an https:// URL"),
            "{}",
            failure.message
        );
    }

    #[tokio::test]
    async fn open_url_uses_the_opener() {
        let tmp = tempfile::tempdir().unwrap();
        let (opener, opened) = recording_opener();
        let ctx = EffectContext {
            paths: Paths::all_in(tmp.path()),
            opener,
        };
        let url = Url::parse("https://portal.azure.com/").unwrap();
        assert!(
            run(to_effect(Action::OpenUrl(url.clone()), &ctx))
                .await
                .is_empty()
        );
        assert_eq!(*opened.lock().unwrap(), vec![url]);
    }

    #[tokio::test]
    async fn run_login_refuses_a_configured_address_before_the_browser() {
        let tmp = tempfile::tempdir().unwrap();
        let config = format!(
            "{FILE_BACKEND}\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n"
        );
        let paths = tmp_paths(&tmp, &config);
        let (opener, opened) = recording_opener();
        let ctx = EffectContext { paths, opener };
        let events = run(to_effect(Action::RunLogin(account("ME@example.com")), &ctx)).await;
        let [Event::LoginFinished(Err(failure))] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert!(
            failure.message.contains("already configured"),
            "{}",
            failure.message
        );
        assert_eq!(failure.class, ErrorClass::Permanent);
        assert!(
            opened.lock().unwrap().is_empty(),
            "no browser for a refused account"
        );
    }

    #[tokio::test]
    async fn persist_writes_tokens_store_and_config_then_reports_done() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = tmp_paths(&tmp, FILE_BACKEND);
        let action = Action::Persist {
            account: account("me@example.com"),
            tokens: tokens(),
        };
        let events = run(to_effect(action, &ctx(paths.clone()))).await;
        let [Event::Persisted(Ok(report))] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert_eq!(report.config_path, paths.config_file());
        assert_eq!(report.token_backend, "file");
        assert_eq!(
            report.token_location,
            paths.secrets_file().display().to_string()
        );
        assert_eq!(report.store_path, paths.db_file());
        let secrets = FileSecretStore::new(paths.secrets_file());
        assert_eq!(
            secrets.get_tokens("me@example.com").unwrap(),
            Some(tokens())
        );
        assert_eq!(
            Store::open(&paths.db_file())
                .unwrap()
                .list_accounts()
                .unwrap()
                .len(),
            1
        );
        let saved = rail_core::config::load(&paths.config_file()).unwrap();
        assert_eq!(saved.accounts[0].address, "me@example.com");
    }

    #[tokio::test]
    async fn a_persist_failure_names_the_cause_and_writes_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = tmp_paths(&tmp, FILE_BACKEND);
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(paths.secrets_file(), "{not json").unwrap();
        let action = Action::Persist {
            account: account("me@example.com"),
            tokens: tokens(),
        };
        let events = run(to_effect(action, &ctx(paths.clone()))).await;
        let [Event::Persisted(Err(failure))] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert!(
            failure.message.contains("storing tokens"),
            "{}",
            failure.message
        );
        assert!(failure.message.contains("corrupt"), "{}", failure.message);
        assert_eq!(failure.class, ErrorClass::Permanent);
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            FILE_BACKEND
        );
    }

    #[test]
    fn operation_errors_become_failures_with_their_class_and_hint() {
        let transient = OpError::new(ErrorCode::Secrets, ErrorClass::Transient, "disk busy")
            .with_hint("try again");
        let shown = failure(&transient);
        assert_eq!(shown.class, ErrorClass::Transient);
        assert_eq!(shown.message, "disk busy\nhint: try again");
        assert!(!shown.consent_required);
        let consent = OpError::new(
            ErrorCode::ConsentRequired,
            ErrorClass::Permanent,
            "AADSTS65001",
        );
        assert!(failure(&consent).consent_required);
    }
}
