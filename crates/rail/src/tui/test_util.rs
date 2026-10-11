//! Helpers shared by the TUI unit tests.

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rail_core::config::{AccountConfig, AccountKind, Config};
use rail_core::daemon::{
    BeginGate, Connector, DaemonContext, Opener as DaemonOpener, SaveGate, ServeConfig, Server,
    SpawnFn,
};
use rail_core::onboarding::{Event, Wizard};
use rail_core::ops::{BrowserEnv, OpError};
use rail_core::paths::Paths;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use serde_json::json;
use tokio::sync::mpsc;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::app::{Clipboard, Effect, Msg, TaskResult};
use super::effects::{EffectContext, Opener};
use super::screen::Screen;
use super::screens::wizard::WizardScreen;
use super::theme::Theme;

/// Where the in-process test daemons send token refreshes and Gmail calls: nothing listens.
const UNREACHABLE_TOKEN_URL: &str = "http://127.0.0.1:9/token";
const UNREACHABLE_GMAIL_URL: &str = "http://127.0.0.1:9/gmail/v1/users/me/";

/// The two sizes every rendering test checks: a classic terminal and a roomy one.
pub const SIZES: [(u16, u16); 2] = [(80, 24), (120, 40)];

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Draw with `draw` on a `width` x `height` test terminal and return the visible rows.
pub fn render_lines(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(draw).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// Records copies instead of touching the real clipboard.
#[derive(Clone, Default)]
pub struct RecordingClipboard(pub Arc<Mutex<Vec<String>>>);

impl Clipboard for RecordingClipboard {
    fn copy(&mut self, text: &str) -> io::Result<()> {
        self.0
            .lock()
            .expect("clipboard lock")
            .push(text.to_string());
        Ok(())
    }
}

/// A screen that says hello, remembers the keys and copy reports it receives, and answers
/// keys with `on_key`.
pub struct Hello {
    pub typing: bool,
    pub keys: Vec<KeyEvent>,
    /// Every `TaskResult::Copied` the screen was told about.
    pub copied: Vec<Result<(), String>>,
    pub on_key: Box<dyn FnMut(KeyEvent) -> Vec<Effect>>,
}

impl Default for Hello {
    fn default() -> Self {
        Self {
            typing: false,
            keys: Vec::new(),
            copied: Vec::new(),
            on_key: Box::new(|_| Vec::new()),
        }
    }
}

impl Screen for Hello {
    fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                self.keys.push(key);
                (self.on_key)(key)
            }
            Msg::Task(TaskResult::Copied(result)) => {
                self.copied.push(result);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn view(&self, frame: &mut Frame, theme: &Theme) {
        frame.render_widget(
            Paragraph::new("hello from rail").style(theme.text),
            frame.area(),
        );
    }

    fn captures_text(&self) -> bool {
        self.typing
    }
}

/// A config with one account, "work" <work@example.com>.
pub fn existing_config() -> Config {
    Config {
        accounts: vec![AccountConfig {
            name: "work".into(),
            kind: AccountKind::Gmail,
            address: "work@example.com".into(),
            color: None,
            client_id: "cid".into(),
            client_secret: None,
            tenant: None,
            signature: None,
            ai_send: None,
            history_days: None,
            body_cache_days: None,
        }],
        ..Config::default()
    }
}

/// For screens whose effects are inspected, never run: the paths do not exist, the opener
/// does nothing and there is no daemon.
pub fn test_ctx() -> EffectContext {
    let opener: Opener = Arc::new(|_: &Url| Ok(()));
    let paths = Paths::all_in(Path::new("/nonexistent/rail-test"));
    let spawn: SpawnFn = Arc::new(|| Err(OpError::internal("no daemon in this test")));
    EffectContext::with(opener, Connector::new(paths, spawn))
}

/// A context backed by a real in-process daemon over `paths`. The daemon's opener sends every
/// login and consent URL to the receiver instead of starting a browser; `token_url` replaces
/// the provider's token endpoint.
pub async fn daemon_ctx(
    paths: &Paths,
    token_url: Option<String>,
) -> (EffectContext, mpsc::UnboundedReceiver<Url>) {
    let (ctx, urls, _) = gated_daemon_ctx(paths, token_url).await;
    (ctx, urls)
}

/// The in-process daemon's hooks that hold its begins and saves part way (disarmed).
pub struct Gates {
    pub begin: Arc<BeginGate>,
    pub save: Arc<SaveGate>,
}

/// `daemon_ctx`, with the daemon's gates.
pub async fn gated_daemon_ctx(
    paths: &Paths,
    token_url: Option<String>,
) -> (EffectContext, mpsc::UnboundedReceiver<Url>, Gates) {
    let (tx, rx) = mpsc::unbounded_channel();
    let daemon_opener: DaemonOpener = Arc::new(move |url: &Url, _: &BrowserEnv| {
        let _ = tx.send(url.clone());
        Ok(())
    });
    // Never Google: an account the test configures syncs against unreachable addresses.
    let ctx = DaemonContext::new(paths.clone(), daemon_opener)
        .expect("daemon context")
        .with_token_url(Some(
            token_url.unwrap_or_else(|| UNREACHABLE_TOKEN_URL.to_string()),
        ))
        .with_gmail_url(Some(UNREACHABLE_GMAIL_URL.to_string()));
    let gates = Gates {
        begin: ctx.begin_gate.clone(),
        save: ctx.save_gate.clone(),
    };
    let server = Server::bind(ctx, ServeConfig::default())
        .await
        .expect("bind the test daemon");
    tokio::spawn(server.run());
    let portal: Opener = Arc::new(|_: &Url| Ok(()));
    let spawn: SpawnFn = Arc::new(|| Ok(()));
    let connector = Connector::new(paths.clone(), spawn);
    (EffectContext::with(portal, connector), rx, gates)
}

/// A token endpoint that answers every exchange with `access` and `refresh`.
pub async fn token_server(access: &str, refresh: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access, "refresh_token": refresh, "expires_in": 3600, "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    server
}

pub fn query(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .expect("the URL carries the parameter")
}

/// Play the browser for `url`: follow its redirect back with an authorization code.
pub async fn redirect(url: &Url) {
    let redirect = query(url, "redirect_uri");
    reqwest::get(format!(
        "{redirect}/?code=c0de&state={}",
        query(url, "state")
    ))
    .await
    .expect("the loopback listener answered");
}

/// Play the browser for the next URL the daemon opens, waiting at most ten seconds for it.
pub fn redirect_when_asked(mut urls: mpsc::UnboundedReceiver<Url>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let url = tokio::time::timeout(Duration::from_secs(10), urls.recv())
            .await
            .expect("the daemon opened a URL in time")
            .expect("the opener channel is open");
        redirect(&url).await;
    })
}

pub fn wizard_screen() -> WizardScreen {
    WizardScreen::new(Wizard::new(Some(existing_config()), None), test_ctx())
}

pub fn press(screen: &mut WizardScreen, code: KeyCode) -> Vec<Effect> {
    screen.update(Msg::Key(key(code)))
}

pub fn type_text(screen: &mut WizardScreen, text: &str) {
    for c in text.chars() {
        press(screen, KeyCode::Char(c));
    }
}

pub fn feed(screen: &mut WizardScreen, event: Event) -> Vec<Effect> {
    screen.update(Msg::Task(TaskResult::Wizard(event)))
}

/// From Start to the Gmail login: own app, Gmail, past the instructions, then the address,
/// client ID and secret, and Enter.
pub fn gmail_own_app_keys(address: &str) -> Vec<KeyEvent> {
    let mut keys = vec![
        key(KeyCode::Down),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
    ];
    keys.extend(address.chars().map(|c| key(KeyCode::Char(c))));
    keys.extend([key(KeyCode::Tab), key(KeyCode::Tab), key(KeyCode::Tab)]);
    keys.extend("cid".chars().map(|c| key(KeyCode::Char(c))));
    keys.push(key(KeyCode::Tab));
    keys.extend("sec".chars().map(|c| key(KeyCode::Char(c))));
    keys.push(key(KeyCode::Enter));
    keys
}
