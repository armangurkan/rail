//! Runs the wizard's `Action`s as daemon requests and reports each outcome back as a wizard
//! `Event`. The daemon runs the login and saves the account when it completes; the wizard's
//! state machine is unchanged, so its `Persist` step only collects what the daemon saved.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rail_core::auth::TokenSet;
use rail_core::config::AccountConfig;
use rail_core::daemon::{Connector, DaemonClient};
use rail_core::error::ErrorClass;
use rail_core::onboarding::{Action, DoneReport, Event, Failure, Preset, PresetEntry};
use rail_core::ops::op::{
    AccountAddBegin, ConsentBegin, Operation, PendingCancel, PendingWait, PresetFetch, PresetsList,
};
use rail_core::ops::{
    AccountAddRequest, BeginResponse, CancelRequest, ClientSecretInput, ConsentBeginRequest,
    ErrorCode, OpError, PendingId, PendingOutcome, PendingState, PresetFetchRequest, PresetSummary,
    PresetsListRequest, SavedAccount, WaitRequest, WaitResponse,
};
use rail_core::paths::Paths;
use tokio::sync::Notify;
use url::Url;

use crate::cmd::account::{CANCEL_TIMEOUT, error_text};
use crate::tui::app::{Effect, TaskResult, TaskSink};
use crate::{browser, daemon_link};

/// Starts the browser on a URL. Injected so tests never start a real one.
pub type Opener = Arc<dyn Fn(&Url) -> io::Result<()> + Send + Sync>;

/// Stands in for a preset's client secret in the wizard's draft. The daemon never returns the
/// secret; the login then names the preset as the source, and the daemon reads it itself.
const PRESET_SECRET: &str = "rail-preset-secret:";

/// The logins and consents this process has begun in the daemon and not seen finish.
#[derive(Default)]
struct Outstanding {
    ids: HashSet<PendingId>,
    /// Begin requests sent whose answer (and so whose id) has not arrived yet.
    begins: usize,
}

/// What the effects need from the outside world: how to open a portal page, and the daemon.
#[derive(Clone)]
pub struct EffectContext {
    /// Portal and help pages only; the daemon opens login pages.
    pub opener: Opener,
    pub daemon: Connector,
    /// What is in flight in the daemon, so a cancel or a quit can end it there.
    outstanding: Arc<Mutex<Outstanding>>,
    /// Told whenever a begin request is answered.
    answered: Arc<Notify>,
    /// What the daemon saved when the last login finished, for the wizard's `Persist` step.
    saved: Arc<Mutex<Option<SavedAccount>>>,
    /// The longest `settle` waits for the daemon.
    cancel_timeout: Duration,
}

impl EffectContext {
    /// The real context: the platform browser and the user's daemon.
    pub fn new(paths: Paths) -> Self {
        Self::with(Arc::new(browser::launch), daemon_link::connector(&paths))
    }

    pub fn with(opener: Opener, daemon: Connector) -> Self {
        Self {
            opener,
            daemon,
            outstanding: Arc::default(),
            answered: Arc::default(),
            saved: Arc::default(),
            cancel_timeout: CANCEL_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_cancel_timeout(mut self, timeout: Duration) -> Self {
        self.cancel_timeout = timeout;
        self
    }

    /// Cancel every login or consent still in flight, waiting first for begins the daemon has
    /// not answered yet. Called once the event loop ends, so quitting mid-login leaves nothing
    /// waiting in the daemon. Bounded: a daemon that does not answer cannot hold the exit.
    pub async fn settle(&self) {
        if tokio::time::timeout(self.cancel_timeout, self.cancel_outstanding())
            .await
            .is_err()
        {
            tracing::warn!(
                "the daemon did not confirm the cancel in time; a login may still be pending"
            );
        }
    }

    async fn cancel_outstanding(&self) {
        self.begins_answered().await;
        let ids: Vec<PendingId> = lock(&self.outstanding).ids.iter().cloned().collect();
        for id in ids {
            cancel_in_daemon(self, id).await;
        }
    }

    async fn begins_answered(&self) {
        loop {
            let answered = self.answered.notified();
            tokio::pin!(answered);
            answered.as_mut().enable();
            if lock(&self.outstanding).begins == 0 {
                return;
            }
            answered.await;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ask the daemon to cancel `id`, then forget it. A daemon that cannot be reached has nothing
/// left to cancel for this process; an id the daemon no longer knows has already finished.
async fn cancel_in_daemon(ctx: &EffectContext, id: PendingId) {
    match ctx.daemon.client().await {
        Ok(client) => {
            let request = CancelRequest { id: id.clone() };
            if let Err(e) = client.call::<PendingCancel>(&request).await {
                tracing::debug!(id = %id, error = %e.message, "the cancel was refused");
            }
        }
        Err(e) => tracing::debug!(id = %id, error = %e.message, "no daemon to cancel in"),
    }
    lock(&ctx.outstanding).ids.remove(&id);
}

/// Counts a begin request as unanswered while it lives.
struct Answering(EffectContext);

impl Answering {
    fn new(ctx: &EffectContext) -> Self {
        lock(&ctx.outstanding).begins += 1;
        Self(ctx.clone())
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        lock(&self.0.outstanding).begins -= 1;
        self.0.answered.notify_waiters();
    }
}

/// Marks a begun operation as in flight. Dropped before `disarm` (the job was aborted by a
/// cancel or a quit), it asks the daemon to cancel the operation; `settle` cancels it too if
/// that cannot finish before the process exits.
struct InFlight {
    ctx: EffectContext,
    id: Option<PendingId>,
}

impl InFlight {
    fn new(ctx: &EffectContext, id: &PendingId) -> Self {
        lock(&ctx.outstanding).ids.insert(id.clone());
        Self {
            ctx: ctx.clone(),
            id: Some(id.clone()),
        }
    }

    /// The operation finished: nothing to cancel.
    fn disarm(mut self) {
        if let Some(id) = self.id.take() {
            lock(&self.ctx.outstanding).ids.remove(&id);
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let ctx = self.ctx.clone();
            runtime.spawn(async move { cancel_in_daemon(&ctx, id).await });
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
        Action::LoadPresets => job(move |sink| load_presets(ctx, sink)),
        Action::ReadPreset(path) => job(move |sink| fetch(ctx, absolute(&path), sink)),
        Action::FetchPreset(url) => job(move |sink| fetch(ctx, url, sink)),
        Action::RunAdminConsent { client_id, tenant } => {
            job(move |sink| admin_consent(ctx, client_id, tenant, sink))
        }
        Action::RunLogin(account) => job(move |sink| login(ctx, account, sink)),
        Action::Persist { .. } => job(move |sink| async move { persisted(&ctx, &sink) }),
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

/// A path typed in the wizard, made absolute against this process's working directory: the
/// daemon runs in `/` and refuses relative paths. Left as typed if there is no working
/// directory, so the daemon's refusal explains it.
fn absolute(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// An operation error as the wizard shows it: the message, any outside text escaped and
/// labelled on a line of its own, the hint, its class, and whether Microsoft wants an
/// administrator's consent.
fn failure(error: &OpError) -> Failure {
    Failure {
        message: error_text(error),
        class: error.class,
        consent_required: error.code == ErrorCode::ConsentRequired,
    }
}

async fn client(ctx: &EffectContext) -> Result<DaemonClient, Failure> {
    ctx.daemon.client().await.map_err(|e| failure(&e))
}

/// A preset as the wizard holds it. A client secret the daemon kept is replaced by a marker
/// naming the preset's source.
fn to_preset(summary: PresetSummary, source: &str) -> Preset {
    Preset {
        name: summary.name.map(|n| n.into_inner()),
        kind: summary.kind,
        client_id: summary.client_id.into_inner(),
        tenant: summary.tenant.map(|t| t.into_inner()),
        client_secret: summary
            .has_client_secret
            .then(|| format!("{PRESET_SECRET}{source}")),
        help_url: summary.help_url.map(|u| u.into_inner()),
    }
}

async fn load_presets(ctx: EffectContext, sink: TaskSink) {
    let listed = match client(&ctx).await {
        Ok(client) => client
            .call::<PresetsList>(&PresetsListRequest {})
            .await
            .map_err(|e| failure(&e)),
        Err(failure) => Err(failure),
    };
    let entries = match listed {
        Ok(listed) => listed
            .presets
            .into_iter()
            .map(|listing| {
                let source = listing.source.into_inner();
                PresetEntry {
                    preset: match listing.preset {
                        Some(summary) => Ok(to_preset(summary, &source)),
                        None => Err(listing.error.map(|e| error_text(&e)).unwrap_or_default()),
                    },
                    source: PathBuf::from(source),
                }
            })
            .collect(),
        Err(failure) => vec![PresetEntry {
            source: PathBuf::new(),
            preset: Err(failure.message),
        }],
    };
    send(&sink, Event::PresetsLoaded(entries));
}

/// A typed path or URL; the daemon refuses plain http before any request.
async fn fetch(ctx: EffectContext, source: String, sink: TaskSink) {
    let result = match client(&ctx).await {
        Ok(client) => client
            .call::<PresetFetch>(&PresetFetchRequest { source })
            .await
            .map(|fetched| to_preset(fetched.preset, &fetched.source))
            .map_err(|e| failure(&e)),
        Err(failure) => Err(failure),
    };
    send(&sink, Event::PresetFetched(result));
}

/// The account the wizard built, as an add request: a preset marker names the preset, a
/// client secret typed as `op://...` is a 1Password reference, anything else is the value.
fn add_request(account: &AccountConfig) -> AccountAddRequest {
    let client_secret =
        account
            .client_secret
            .clone()
            .map(|secret| match secret.strip_prefix(PRESET_SECRET) {
                Some(source) => ClientSecretInput::Preset(source.to_string()),
                None => ClientSecretInput::from_flag(secret),
            });
    AccountAddRequest {
        kind: account.kind,
        address: account.address.clone(),
        client_id: Some(account.client_id.clone()),
        client_secret,
        tenant: account.tenant.clone(),
        name: Some(account.name.clone()),
        color: account.color.clone(),
        store_secret_in_1password: false,
        open_browser: true,
    }
}

/// Send a begin request in a task of its own, so a job aborted while the daemon answers still
/// learns the id: the task's unread output, the `InFlight`, is then dropped and cancels it.
async fn begin<O>(
    ctx: &EffectContext,
    client: &DaemonClient,
    request: O::Request,
) -> Result<(BeginResponse, InFlight), Failure>
where
    O: Operation<Response = BeginResponse>,
{
    let answering = Answering::new(ctx);
    let (ctx, client) = (ctx.clone(), client.clone());
    let task = tokio::spawn(async move {
        let _answering = answering;
        let begun = client.call::<O>(&request).await?;
        let in_flight = InFlight::new(&ctx, &begun.id);
        Ok::<_, OpError>((begun, in_flight))
    });
    match task.await {
        Ok(result) => result.map_err(|e| failure(&e)),
        Err(e) => Err(Failure::new(
            format!("internal error: {e}"),
            ErrorClass::Permanent,
        )),
    }
}

/// `pending.wait` until the operation finishes.
async fn finished(client: &DaemonClient, id: &PendingId) -> Result<WaitResponse, Failure> {
    let wait = WaitRequest {
        id: id.clone(),
        timeout_secs: None,
    };
    loop {
        let answer = client
            .call::<PendingWait>(&wait)
            .await
            .map_err(|e| failure(&e))?;
        if answer.state.is_finished() {
            return Ok(answer);
        }
    }
}

/// Begin the add in the daemon, show the URL, wait. The daemon saves the account as soon as
/// the login completes; the wizard then gets tokens that are only a placeholder, because the
/// real ones never leave the daemon.
async fn login(ctx: EffectContext, account: AccountConfig, sink: TaskSink) {
    let result = run_login(&ctx, &account, &sink).await;
    send(&sink, Event::LoginFinished(result));
}

async fn run_login(
    ctx: &EffectContext,
    account: &AccountConfig,
    sink: &TaskSink,
) -> Result<TokenSet, Failure> {
    let client = client(ctx).await?;
    let (begun, in_flight) = begin::<AccountAddBegin>(ctx, &client, add_request(account)).await?;
    if let Ok(url) = Url::parse(&begun.url) {
        send(sink, Event::LoginUrl(url));
    }
    let answer = finished(&client, &begun.id).await?;
    in_flight.disarm();
    match answer.state {
        PendingState::Done {
            outcome: PendingOutcome::Added { account },
        } => {
            *lock(&ctx.saved) = Some(account);
            Ok(saved_by_the_daemon())
        }
        PendingState::Failed { error } => Err(failure(&error)),
        other => Err(ended_without(&other, "the login ended without saving")),
    }
}

/// A finished operation that neither succeeded nor failed: it was cancelled, or the daemon
/// answered with an outcome of another kind.
fn ended_without(state: &PendingState, what: &str) -> Failure {
    let why = match state {
        PendingState::Cancelled => "it was cancelled",
        _ => "the daemon reported another kind of outcome",
    };
    Failure::new(format!("{what}: {why}"), ErrorClass::Permanent)
}

/// Stands for the tokens the daemon stored. The wizard only needs to know the login worked.
fn saved_by_the_daemon() -> TokenSet {
    TokenSet {
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at: 0,
    }
}

/// The wizard's `Persist`: report what the daemon saved when the login finished.
fn persisted(ctx: &EffectContext, sink: &TaskSink) {
    let result = match lock(&ctx.saved).take() {
        Some(saved) => Ok(DoneReport {
            config_path: saved.config_path,
            token_backend: saved.token_backend,
            token_location: saved.token_location,
            store_path: saved.store_path,
        }),
        None => Err(Failure::new(
            "the daemon has no saved account to report; go back and log in again",
            ErrorClass::Permanent,
        )),
    };
    send(sink, Event::Persisted(result));
}

async fn admin_consent(ctx: EffectContext, client_id: String, tenant: String, sink: TaskSink) {
    let result = run_consent(&ctx, client_id, tenant, &sink).await;
    send(&sink, Event::AdminConsentFinished(result));
}

async fn run_consent(
    ctx: &EffectContext,
    client_id: String,
    tenant: String,
    sink: &TaskSink,
) -> Result<String, Failure> {
    let client = client(ctx).await?;
    let request = ConsentBeginRequest {
        client_id,
        tenant,
        open_browser: true,
    };
    let (begun, in_flight) = begin::<ConsentBegin>(ctx, &client, request).await?;
    if let Ok(url) = Url::parse(&begun.url) {
        send(sink, Event::ConsentUrl(url));
    }
    let answer = finished(&client, &begun.id).await?;
    in_flight.disarm();
    match answer.state {
        PendingState::Done {
            outcome: PendingOutcome::Consented { tenant },
        } => Ok(tenant.into_inner()),
        PendingState::Failed { error } => Err(failure(&error)),
        other => Err(ended_without(&other, "the consent ended without an answer")),
    }
}

#[cfg(test)]
mod tests;
