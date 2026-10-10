//! The onboarding wizard screen. Keys become wizard `Input`s, wizard `Action`s become
//! effects, and effect results come back as wizard `Event`s. Drawing is in `wizard_view`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rail_core::error::ErrorClass;
use rail_core::onboarding::{
    Action, Body, Busy, Event, Failure, Field, FieldView, Input, PresetEntry, Primary, Step,
    ViewModel, Wizard,
};
use ratatui::Frame;
use url::Url;

use crate::tui::app::{Effect, Msg, TaskResult};
use crate::tui::effects::{EffectContext, to_effect};
use crate::tui::screen::Screen;
use crate::tui::theme::Theme;
use crate::tui::widgets::scroll_panel::{ScrollPanel, line_count};
use crate::tui::widgets::select::{Select, SelectOutcome};
use crate::tui::widgets::text_input::TextInput;

/// The instructions panel's text width and height before the first frame is drawn.
const FALLBACK_PANEL: (u16, u16) = (57, 10);

/// What keys mean right now.
/// The copy notice: the clipboard escape cannot confirm delivery, so it says where to look.
/// The status line is one row, so both notices stay under 80 columns.
const COPY_NOTICE: &str = "Sent via OSC 52. If not copied, select the URL on screen";
/// The consent URL is the only one printed on exit, so only its notice says so.
const COPY_CONSENT_NOTICE: &str =
    "Sent via OSC 52. If not copied, select the URL; it is also printed on quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Choices,
    Fields,
    /// Carries the instructions' height in wrapped rows.
    Instructions(u16),
    Message,
}

/// The wizard on screen. No `Debug`: the inputs and the wizard hold the client secret.
pub struct WizardScreen {
    pub(super) wizard: Wizard,
    ctx: EffectContext,
    pub(super) model: ViewModel,
    /// Edit state (cursor, masking) per text field; the wizard owns the values.
    pub(super) inputs: BTreeMap<Field, TextInput>,
    pub(super) focus: usize,
    pub(super) select: Select,
    pub(super) scroll: ScrollPanel,
    /// The instructions panel's text width and visible height from the last frame drawn,
    /// so scrolling counts rows the way the screen wraps them.
    pub(super) panel: Cell<(u16, u16)>,
    pub(super) tick: usize,
    /// The outcome of a copy, shown until the next key.
    pub(super) notice: Option<String>,
    /// A job that panicked with nothing in flight, shown until the next key.
    pub(super) failure: Option<String>,
    /// Ctrl-C arrived while saving; quit once the save reports back.
    quit_after_save: bool,
    step: Step,
}

impl WizardScreen {
    /// A screen showing `wizard`, running its actions with `ctx`.
    pub fn new(wizard: Wizard, ctx: EffectContext) -> Self {
        let model = wizard.view_model();
        let step = wizard.step();
        let select = Select::new(choices(&model.body));
        let mut screen = Self {
            wizard,
            ctx,
            model,
            inputs: BTreeMap::new(),
            focus: 0,
            select,
            scroll: ScrollPanel::default(),
            panel: Cell::new((0, 0)),
            tick: 0,
            notice: None,
            failure: None,
            quit_after_save: false,
            step,
        };
        screen.sync_inputs();
        screen
    }

    /// Names of the accounts saved in this session.
    pub fn added(&self) -> &[String] {
        self.wizard.added()
    }

    /// True when the wizard ended on its final step, as opposed to being cancelled midway.
    pub fn finished(&self) -> bool {
        self.wizard.step() == Step::Done
    }

    /// The last consent and login URLs shown, for the caller to print once the terminal is back.
    pub fn shown_urls(&self) -> (Option<Url>, Option<Url>) {
        self.wizard.shown_urls()
    }

    /// True while the account is being written: nothing may cancel that.
    pub(super) fn saving(&self) -> bool {
        self.wizard.busy() == Some(Busy::Persisting)
    }

    fn handle_inputs(&mut self, inputs: Vec<Input>) -> Vec<Effect> {
        let mut actions = Vec::new();
        for input in inputs {
            actions.extend(self.wizard.handle(input));
        }
        self.refresh();
        self.effects(actions)
    }

    /// Apply an outcome; a quit deferred by the save happens once the save has reported.
    fn apply_event(&mut self, event: Event) -> Vec<Effect> {
        let actions = self.wizard.apply(event);
        self.refresh();
        let mut effects = self.effects(actions);
        if self.quit_after_save && !self.saving() {
            effects.push(Effect::Quit);
        }
        effects
    }

    /// Ruling: a panicked job is the failure of whatever action is in flight, and that job is
    /// cancelled so it cannot deliver later. While saving, the panic is only logged: it may
    /// come from a sibling job, and the save is never abandoned.
    fn job_panicked(&mut self, message: &str) -> Vec<Effect> {
        tracing::error!(panic = %message, "a wizard job panicked");
        if self.saving() {
            return Vec::new();
        }
        let failure = Failure::new(
            format!("internal error: {message}; see rail.log"),
            ErrorClass::Permanent,
        );
        let event = match self.wizard.busy() {
            Some(Busy::LoadingPresets) => Event::PresetsLoaded(vec![PresetEntry {
                source: PathBuf::new(),
                preset: Err(failure.message),
            }]),
            Some(Busy::FetchingPreset) => Event::PresetFetched(Err(failure)),
            Some(Busy::Consent) => Event::AdminConsentFinished(Err(failure)),
            Some(Busy::Login) => Event::LoginFinished(Err(failure)),
            Some(Busy::Persisting) => return Vec::new(),
            None => {
                self.failure = Some(format!("Error: {}", failure.message));
                return Vec::new();
            }
        };
        let mut effects = vec![Effect::CancelAll];
        effects.extend(self.apply_event(event));
        effects
    }

    fn copied(&mut self, result: Result<(), String>) {
        // Only the consent URL survives a quit; the login URL dies with this process.
        let consent = self.shows_consent_url();
        self.notice = Some(match result {
            Ok(()) if consent => COPY_CONSENT_NOTICE.to_string(),
            Ok(()) => COPY_NOTICE.to_string(),
            Err(e) => format!("Could not copy: {e}; the URL is shown above"),
        });
    }

    fn effects(&self, actions: Vec<Action>) -> Vec<Effect> {
        actions
            .into_iter()
            .map(|action| to_effect(action, &self.ctx))
            .collect()
    }

    /// Re-read the view model; a new step resets focus, scroll and the list selection.
    fn refresh(&mut self) {
        self.model = self.wizard.view_model();
        let items = choices(&self.model.body);
        if self.wizard.step() == self.step {
            self.select.set_items(items);
        } else {
            self.step = self.wizard.step();
            self.focus = 0;
            self.scroll = ScrollPanel::default();
            self.select = Select::new(items);
        }
        self.sync_inputs();
    }

    /// Text the wizard changed itself (a derived name, a preset's tenant) reaches the inputs.
    fn sync_inputs(&mut self) {
        for view in text_fields(&self.model.body) {
            let input = self.inputs.entry(view.field).or_default();
            input.set_masked(view.masked);
            if input.value() != view.value {
                input.set_value(&view.value);
            }
        }
    }

    pub(super) fn focused_field(&self) -> Option<&FieldView> {
        match &self.model.body {
            Body::Fields(fields) => fields.get(self.focus),
            Body::Presets { source, .. } if self.focus == 1 => Some(source),
            _ => None,
        }
    }

    /// The instructions panel's text width and visible height.
    fn panel_size(&self) -> (u16, u16) {
        match self.panel.get() {
            (0, _) | (_, 0) => FALLBACK_PANEL,
            size => size,
        }
    }

    pub(super) fn mode(&self) -> Mode {
        match &self.model.body {
            Body::Choices(_) | Body::Report { .. } => Mode::Choices,
            Body::Presets { .. } if self.focus == 0 => Mode::Choices,
            Body::Presets { .. } | Body::Fields(_) => Mode::Fields,
            Body::Instructions { text, .. } => {
                Mode::Instructions(line_count(text, self.panel_size().0))
            }
            Body::Message(_) => Mode::Message,
        }
    }

    fn focus_count(&self) -> usize {
        match &self.model.body {
            Body::Fields(fields) => fields.len().max(1),
            Body::Presets { .. } => 2,
            _ => 1,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Input> {
        self.notice = None;
        self.failure = None;
        if self.saving() {
            self.quit_after_save |= is_ctrl_c(key);
            return Vec::new();
        }
        match key.code {
            KeyCode::Esc => return vec![Input::Back],
            KeyCode::F(1) => return vec![Input::OpenPortal],
            KeyCode::Tab | KeyCode::BackTab => {
                self.move_focus(key.code == KeyCode::Tab);
                return Vec::new();
            }
            _ => {}
        }
        if !self.captures_text()
            && let Some(input) = self.failure_key(key)
        {
            return vec![input];
        }
        match self.mode() {
            Mode::Choices => self.choose_key(key),
            Mode::Fields => self.field_key(key),
            Mode::Instructions(lines) => self.instructions_key(key, lines),
            Mode::Message if key.code == KeyCode::Enter => vec![Input::Next],
            Mode::Message => Vec::new(),
        }
    }

    fn failure_key(&self, key: KeyEvent) -> Option<Input> {
        let failure = self.model.failure.as_ref();
        match key.code {
            KeyCode::Char('r') if failure.is_some() => Some(Input::Retry),
            KeyCode::Char('c') if self.can_copy_url() => Some(Input::CopyConsentUrl),
            _ => None,
        }
    }

    /// True while a failure offers the consent URL to copy.
    fn shows_consent_url(&self) -> bool {
        (self.model.failure.as_ref()).is_some_and(|f| matches!(f.primary, Primary::CopyUrl(_)))
    }

    /// The consent URL of a failure, or the login URL while the Login step shows one.
    fn can_copy_url(&self) -> bool {
        let consent = self.shows_consent_url();
        consent || (self.step == Step::Login && self.model.url.is_some())
    }

    fn choose_key(&mut self, key: KeyEvent) -> Vec<Input> {
        match self.select.handle_key(key) {
            SelectOutcome::Chosen(index) => vec![Input::Choose(index)],
            SelectOutcome::Moved | SelectOutcome::Ignored => Vec::new(),
        }
    }

    fn instructions_key(&mut self, key: KeyEvent, lines: u16) -> Vec<Input> {
        match key.code {
            KeyCode::Enter => vec![Input::Next],
            KeyCode::Char('o') => vec![Input::OpenPortal],
            _ => {
                let page = self.panel_size().1;
                self.scroll.handle_key(key, lines, page);
                Vec::new()
            }
        }
    }

    fn field_key(&mut self, key: KeyEvent) -> Vec<Input> {
        let Some(view) = self.focused_field() else {
            return Vec::new();
        };
        let (field, checkbox) = (view.field, view.checkbox.is_some());
        match key.code {
            KeyCode::Enter => vec![Input::Next],
            KeyCode::Up | KeyCode::Down => {
                self.move_focus(key.code == KeyCode::Down);
                Vec::new()
            }
            KeyCode::Char(' ') if checkbox => vec![Input::ToggleAdmin],
            _ if checkbox => Vec::new(),
            _ => self.edit(field, key),
        }
    }

    fn edit(&mut self, field: Field, key: KeyEvent) -> Vec<Input> {
        let input = self.inputs.entry(field).or_default();
        if input.handle_key(key) {
            vec![Input::Edit(field, input.value().to_string())]
        } else {
            Vec::new()
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let count = self.focus_count();
        self.focus = if forward {
            (self.focus + 1) % count
        } else {
            (self.focus + count - 1) % count
        };
    }
}

impl Screen for WizardScreen {
    fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                let inputs = self.on_key(key);
                self.handle_inputs(inputs)
            }
            Msg::Tick => {
                self.tick = self.tick.wrapping_add(1);
                Vec::new()
            }
            Msg::Resize(..) => Vec::new(),
            Msg::Task(TaskResult::Wizard(event)) => self.apply_event(event),
            Msg::Task(TaskResult::JobPanicked(message)) => self.job_panicked(&message),
            Msg::Task(TaskResult::Copied(result)) => {
                self.copied(result);
                Vec::new()
            }
        }
    }

    fn view(&self, frame: &mut Frame, theme: &Theme) {
        self.render(frame, theme);
    }

    fn captures_text(&self) -> bool {
        self.focused_field()
            .is_some_and(|view| view.checkbox.is_none())
    }

    fn defers_quit(&self) -> bool {
        self.saving()
    }
}

fn is_ctrl_c(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn choices(body: &Body) -> Vec<String> {
    match body {
        Body::Choices(items)
        | Body::Presets { choices: items, .. }
        | Body::Report { choices: items, .. } => items.clone(),
        _ => Vec::new(),
    }
}

fn text_fields(body: &Body) -> Vec<FieldView> {
    match body {
        Body::Fields(fields) => fields
            .iter()
            .filter(|view| view.checkbox.is_none())
            .cloned()
            .collect(),
        Body::Presets { source, .. } => vec![source.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    use crossterm::event::Event as TermEvent;
    use rail_core::auth::{FileSecretStore, TokenSet};
    use rail_core::error::ErrorClass;
    use rail_core::onboarding::{Busy, DoneReport, Failure};
    use rail_core::paths::Paths;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;
    use url::Url;

    use crate::tui::app::{App, update};
    use crate::tui::effects::Opener;
    use crate::tui::run::run_loop;
    use crate::tui::test_util::{
        RecordingClipboard, ctrl, feed, gmail_own_app_keys, key, press, type_text, wizard_screen,
    };

    fn drive(screen: &mut WizardScreen, keys: Vec<KeyEvent>) -> Vec<Effect> {
        keys.into_iter()
            .flat_map(|k| screen.update(Msg::Key(k)))
            .collect()
    }

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1,
        }
    }

    fn report() -> DoneReport {
        DoneReport {
            config_path: "/c/config.toml".into(),
            token_backend: "file".into(),
            token_location: "/d/secrets.json".into(),
            store_path: "/d/rail.db".into(),
        }
    }

    #[test]
    fn enter_on_a_choice_moves_through_the_steps() {
        let mut screen = wizard_screen();
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Enter);
        assert_eq!(screen.wizard.step(), Step::Provider);
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Enter);
        assert_eq!(screen.wizard.step(), Step::Instructions);
        assert_eq!(
            screen.wizard.draft().kind,
            Some(rail_core::config::AccountKind::M365)
        );
        press(&mut screen, KeyCode::Esc);
        assert_eq!(screen.wizard.step(), Step::Provider);
    }

    #[test]
    fn typing_edits_the_draft_and_q_is_text_in_a_field() {
        let mut screen = wizard_screen();
        drive(
            &mut screen,
            vec![
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::Enter),
                key(KeyCode::Enter),
            ],
        );
        assert_eq!(screen.wizard.step(), Step::Details);
        assert!(screen.captures_text());
        type_text(&mut screen, "q@x.io");
        assert_eq!(screen.wizard.draft().address, "q@x.io");
        assert_eq!(
            screen.wizard.draft().name,
            "q",
            "the name follows the address"
        );
        press(&mut screen, KeyCode::Tab);
        assert_eq!(
            screen.inputs[&Field::Name].value(),
            "q",
            "programmatic changes reach the input"
        );
    }

    #[test]
    fn space_toggles_the_administrator_box() {
        let mut screen = wizard_screen();
        drive(
            &mut screen,
            vec![
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::Enter),
            ],
        );
        assert_eq!(screen.wizard.step(), Step::Details);
        for _ in 0..6 {
            press(&mut screen, KeyCode::Tab);
        }
        assert!(!screen.captures_text(), "the checkbox takes no text");
        press(&mut screen, KeyCode::Char(' '));
        assert!(screen.wizard.draft().admin);
    }

    #[test]
    fn the_login_starts_a_job_and_esc_cancels_it() {
        let mut screen = wizard_screen();
        let effects = drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        assert_eq!(screen.wizard.step(), Step::Login);
        assert!(
            matches!(effects.last(), Some(Effect::Spawn(_))),
            "{effects:?}"
        );
        let effects = press(&mut screen, KeyCode::Esc);
        assert!(
            matches!(effects.as_slice(), [Effect::CancelAll]),
            "{effects:?}"
        );
        assert_eq!(screen.wizard.step(), Step::Details);
    }

    #[test]
    fn r_retries_and_c_copies_only_when_an_error_shows() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        assert!(
            press(&mut screen, KeyCode::Char('r')).is_empty(),
            "nothing to retry yet"
        );
        let failure = Failure::new("boom", ErrorClass::Transient);
        feed(&mut screen, Event::LoginFinished(Err(failure)));
        let effects = press(&mut screen, KeyCode::Char('r'));
        assert!(
            matches!(effects.as_slice(), [Effect::Spawn(_)]),
            "{effects:?}"
        );
    }

    /// Ruling P1: once saving, Esc and q do nothing, and Ctrl-C waits for the result.
    #[test]
    fn persist_cannot_be_cancelled() {
        let mut app = App::new(wizard_screen(), Box::new(RecordingClipboard::default()));
        for k in gmail_own_app_keys("me@gmail.com") {
            update(&mut app, Msg::Key(k));
        }
        let login = Msg::Task(TaskResult::Wizard(Event::LoginFinished(Ok(tokens()))));
        let effects = update(&mut app, login);
        assert!(
            matches!(effects.as_slice(), [Effect::Spawn(_)]),
            "{effects:?}"
        );
        assert_eq!(app.screen.wizard.busy(), Some(Busy::Persisting));
        for k in [key(KeyCode::Esc), key(KeyCode::Char('q')), ctrl('c')] {
            let effects = update(&mut app, Msg::Key(k));
            assert!(effects.is_empty(), "{k:?} gave {effects:?}");
            assert!(!app.quit, "{k:?} quit while saving");
        }
        assert_eq!(app.screen.wizard.step(), Step::Login);
        assert_eq!(app.screen.wizard.busy(), Some(Busy::Persisting));
        let saved = Msg::Task(TaskResult::Wizard(Event::Persisted(Ok(report()))));
        let effects = update(&mut app, saved);
        assert!(matches!(effects.as_slice(), [Effect::Quit]), "{effects:?}");
        assert_eq!(app.screen.added(), ["me".to_string()]);
    }

    /// Ruling P6: a panicked job fails whatever action was in flight.
    #[test]
    fn a_panicked_job_fails_the_action_in_flight() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        assert_eq!(screen.wizard.busy(), Some(Busy::Login));
        screen.update(Msg::Task(TaskResult::JobPanicked("boom".into())));
        assert_eq!(screen.wizard.busy(), None);
        let failure = screen.wizard.failure().unwrap();
        assert_eq!(failure.message, "internal error: boom; see rail.log");
        assert_eq!(failure.class, ErrorClass::Permanent);
        let effects = press(&mut screen, KeyCode::Char('r'));
        assert!(
            matches!(effects.as_slice(), [Effect::Spawn(_)]),
            "{effects:?}"
        );
    }

    /// A panic while saving may come from a sibling job: the save keeps running, the screen
    /// keeps holding, and the deferred quit still waits for the save's own result.
    #[test]
    fn a_panic_while_saving_is_ignored_and_the_save_still_finishes() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        feed(&mut screen, Event::LoginFinished(Ok(tokens())));
        assert!(screen.update(Msg::Key(ctrl('c'))).is_empty());
        let effects = screen.update(Msg::Task(TaskResult::JobPanicked("disk".into())));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(screen.wizard.busy(), Some(Busy::Persisting));
        assert!(screen.defers_quit(), "still saving");
        assert!(screen.wizard.failure().is_none());
        let effects = screen.update(Msg::Task(TaskResult::Wizard(Event::Persisted(
            Ok(report()),
        ))));
        assert!(matches!(effects.as_slice(), [Effect::Quit]), "{effects:?}");
        assert_eq!(screen.added(), ["me".to_string()]);
    }

    #[test]
    fn a_panic_during_login_cancels_the_job_really_in_flight() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        assert_eq!(screen.wizard.busy(), Some(Busy::Login));
        let effects = screen.update(Msg::Task(TaskResult::JobPanicked("boom".into())));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::CancelAll)),
            "{effects:?}"
        );
        assert!(screen.wizard.failure().is_some());
    }

    #[test]
    fn c_on_the_login_step_copies_the_login_url() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        assert!(
            press(&mut screen, KeyCode::Char('c')).is_empty(),
            "no URL yet"
        );
        let login = Url::parse("https://accounts.example/auth?state=s").unwrap();
        feed(&mut screen, Event::LoginUrl(login.clone()));
        let effects = press(&mut screen, KeyCode::Char('c'));
        assert!(
            matches!(effects.as_slice(), [Effect::Copy(u)] if *u == login.to_string()),
            "{effects:?}"
        );
    }

    #[test]
    fn the_copy_notice_for_the_login_url_does_not_promise_a_printout() {
        let mut screen = wizard_screen();
        drive(&mut screen, gmail_own_app_keys("me@gmail.com"));
        let login = Url::parse("https://accounts.example/auth?state=s").unwrap();
        feed(&mut screen, Event::LoginUrl(login));
        press(&mut screen, KeyCode::Char('c'));
        screen.update(Msg::Task(TaskResult::Copied(Ok(()))));
        let notice = screen.notice.as_deref().unwrap();
        assert!(!notice.contains("printed"), "{notice}");
    }

    #[test]
    fn the_copy_notice_does_not_claim_success_it_cannot_verify() {
        let mut screen = wizard_screen();
        screen.update(Msg::Task(TaskResult::Copied(Ok(()))));
        assert_eq!(
            screen.notice.as_deref(),
            Some("Sent via OSC 52. If not copied, select the URL on screen")
        );
    }

    /// Review focus 2 and 5, in process: the real screen, loop and effects, cancelled while
    /// the login waits for the browser, leave no config change, no tokens and no store row.
    #[tokio::test]
    async fn cancel_during_login_leaves_no_config_tokens_or_store_row() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let config_text = "[general]\nsecrets = \"file\"\n";
        std::fs::write(paths.config_file(), config_text).unwrap();
        let (url_tx, mut url_rx) = mpsc::unbounded_channel::<Url>();
        let opener: Opener = Arc::new(move |url: &Url| {
            let _ = url_tx.send(url.clone());
            Ok(())
        });
        let ctx = EffectContext {
            paths: paths.clone(),
            opener,
        };
        let config = rail_core::config::load(&paths.config_file()).unwrap();
        let screen = WizardScreen::new(Wizard::new(Some(config), None), ctx);
        let mut app = App::new(screen, Box::new(RecordingClipboard::default()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let (key_tx, mut key_rx) = mpsc::unbounded_channel::<io::Result<TermEvent>>();
        let events = futures_util::stream::poll_fn(move |cx| key_rx.poll_recv(cx));
        for k in gmail_own_app_keys("me@example.com") {
            key_tx.send(Ok(TermEvent::Key(k))).unwrap();
        }
        let driver = async move {
            let url = tokio::time::timeout(Duration::from_secs(10), url_rx.recv())
                .await
                .expect("the login reached the browser")
                .expect("the opener was called");
            assert!(url.query_pairs().any(|(k, _)| k == "state"), "{url}");
            key_tx.send(Ok(TermEvent::Key(ctrl('c')))).unwrap();
            key_tx
        };
        let (result, _keys) = tokio::join!(run_loop(&mut terminal, &mut app, events), driver);
        result.unwrap();
        assert!(app.quit);
        assert!(app.screen.added().is_empty());
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            config_text
        );
        assert!(!paths.secrets_file().exists(), "no tokens");
        assert!(!paths.db_file().exists(), "no store row");
        let secrets = FileSecretStore::new(paths.secrets_file());
        assert!(
            rail_core::auth::SecretStore::get_tokens(&secrets, "me@example.com")
                .unwrap()
                .is_none()
        );
    }
}
