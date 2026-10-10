//! Runs the wizard's `Action`s with the code the flag commands already use, and reports
//! each outcome back as a wizard `Event`. Synchronous file and keyring work runs on the
//! blocking pool, so the event loop never waits on I/O.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::Report;
use rail_core::auth::{
    AuthError, SecretError, TokenSet, http_client, open_secret_store, run_admin_consent,
};
use rail_core::config::{AccountConfig, ConfigError, SecretsBackend};
use rail_core::error::ErrorClass;
use rail_core::onboarding::{
    Action, DoneReport, Event, Failure, PresetEntry, discover_presets, fetch_preset, read_preset,
};
use rail_core::paths::Paths;
use rail_core::store::StoreError;
use url::Url;

use crate::browser;
use crate::cmd::account::{self, NewAccount};
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
    let (paths, checked) = (ctx.paths.clone(), account.clone());
    let ready = blocking(move || check_new_account(&paths, checked))
        .await
        .and_then(|checked| checked);
    if let Err(failure) = ready {
        send(&sink, Event::LoginFinished(Err(failure)));
        return;
    }
    let result = account::login(&account, |url| {
        send(&sink, Event::LoginUrl(url.clone()));
        open(&ctx.opener, url);
    })
    .await;
    let result = result.map_err(|e| Failure::from_auth(&e));
    send(&sink, Event::LoginFinished(result));
}

/// What the flag form checks before its browser opens: no duplicate, a config that can take
/// the account, and a reachable secret store.
fn check_new_account(paths: &Paths, account: AccountConfig) -> Result<(), Failure> {
    let pending =
        account::prepare(paths, NewAccount::from(account)).map_err(|e| failure_from(&e))?;
    let secrets = open_secret_store(pending.backend, paths);
    account::probe_secrets(secrets.as_ref(), &pending.account.address)
        .map_err(|e| Failure::new(format!("secret storage is not usable: {e}"), e.class()))
}

async fn persist(paths: Paths, account: AccountConfig, tokens: TokenSet, sink: TaskSink) {
    let result = blocking(move || save_new_account(&paths, account, &tokens))
        .await
        .and_then(|saved| saved);
    send(&sink, Event::Persisted(result));
}

/// The flag form's persist: tokens, then the store row, then config.toml last.
fn save_new_account(
    paths: &Paths,
    account: AccountConfig,
    tokens: &TokenSet,
) -> Result<DoneReport, Failure> {
    let pending =
        account::prepare(paths, NewAccount::from(account)).map_err(|e| failure_from(&e))?;
    let secrets = open_secret_store(pending.backend, paths);
    account::persist(paths, secrets.as_ref(), &pending, tokens).map_err(|e| failure_from(&e))?;
    let token_location = match pending.backend {
        SecretsBackend::Keyring => "OS keyring, service \"rail\"".to_string(),
        SecretsBackend::File => paths.secrets_file().display().to_string(),
    };
    Ok(DoneReport {
        config_path: pending.config_path,
        token_backend: secrets.backend_name().to_string(),
        token_location,
        store_path: paths.db_file(),
    })
}

/// The flag path's error report as the wizard shows it: the same text the CLI prints (the
/// whole cause chain, including the `secrets = "file"` hint), classified by the first typed
/// cause found; an untyped message is permanent.
fn failure_from(report: &Report) -> Failure {
    let class = report
        .chain()
        .find_map(classify)
        .unwrap_or(ErrorClass::Permanent);
    Failure::new(format!("{report:#}"), class)
}

fn classify(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorClass> {
    if let Some(e) = cause.downcast_ref::<AuthError>() {
        return Some(e.class());
    }
    if let Some(e) = cause.downcast_ref::<SecretError>() {
        return Some(e.class());
    }
    if let Some(e) = cause.downcast_ref::<StoreError>() {
        return Some(e.class());
    }
    cause.downcast_ref::<ConfigError>().map(ConfigError::class)
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
    fn failures_are_classified_by_their_first_typed_cause() {
        use color_eyre::eyre::WrapErr;
        let io = SecretError::Io {
            path: "/x".into(),
            source: std::io::Error::other("disk busy"),
        };
        let report = Err::<(), _>(io).wrap_err("storing tokens").unwrap_err();
        let failure = failure_from(&report);
        assert_eq!(failure.class, ErrorClass::Transient);
        assert!(
            failure
                .message
                .starts_with("storing tokens: secrets file /x"),
            "{}",
            failure.message
        );
        let plain = color_eyre::eyre::eyre!("already configured");
        assert_eq!(failure_from(&plain).class, ErrorClass::Permanent);
    }
}
