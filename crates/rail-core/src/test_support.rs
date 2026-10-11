//! Shared fixtures for unit tests.

use crate::auth::TokenSet;
use crate::config::{AccountConfig, AccountKind, Config};
use crate::onboarding::{Action, DoneReport, Field, Input, Preset, Wizard};

pub(crate) fn sample_account() -> AccountConfig {
    AccountConfig {
        name: "work".to_string(),
        kind: AccountKind::Gmail,
        address: "me@example.com".to_string(),
        color: Some("blue".to_string()),
        client_id: "cid".to_string(),
        client_secret: None,
        tenant: None,
        signature: None,
        ai_send: None,
        history_days: None,
        body_cache_days: None,
    }
}

/// A config with one account, "work" <work@example.com>.
pub(crate) fn existing_config() -> Config {
    Config {
        accounts: vec![AccountConfig {
            address: "work@example.com".to_string(),
            ..sample_account()
        }],
        ..Config::default()
    }
}

pub(crate) fn contoso_preset() -> Preset {
    Preset {
        name: Some("Contoso Microsoft 365".to_string()),
        kind: AccountKind::M365,
        client_id: "contoso-app".to_string(),
        tenant: Some("contoso.onmicrosoft.com".to_string()),
        client_secret: None,
        help_url: Some("https://intranet.contoso.example/rail".to_string()),
    }
}

pub(crate) fn wizard() -> Wizard {
    Wizard::new(Some(existing_config()), None)
}

/// Feed `inputs` in order and collect every action they produce.
pub(crate) fn walk(wizard: &mut Wizard, inputs: impl IntoIterator<Item = Input>) -> Vec<Action> {
    inputs.into_iter().flat_map(|i| wizard.handle(i)).collect()
}

pub(crate) fn edit(field: Field, value: &str) -> Input {
    Input::Edit(field, value.to_string())
}

/// From Start to the Details step of the bring-your-own path: 0 is Gmail, 1 Microsoft 365.
pub(crate) fn own_app(provider: usize) -> Vec<Input> {
    vec![Input::Choose(1), Input::Choose(provider), Input::Next]
}

pub(crate) fn gmail_details(address: &str) -> Vec<Input> {
    vec![
        edit(Field::Address, address),
        edit(Field::ClientId, "cid"),
        edit(Field::ClientSecret, "sec"),
    ]
}

pub(crate) fn m365_details(address: &str, tenant: &str) -> Vec<Input> {
    vec![
        edit(Field::Address, address),
        edit(Field::ClientId, "cid"),
        edit(Field::Tenant, tenant),
    ]
}

pub(crate) fn done_report() -> DoneReport {
    DoneReport {
        config_path: "/home/me/.config/rail/config.toml".into(),
        token_backend: "file".to_string(),
        token_location: "/home/me/.local/share/rail/secrets.json".to_string(),
        store_path: "/home/me/.local/share/rail/rail.db".into(),
    }
}

pub(crate) fn tokens() -> TokenSet {
    TokenSet {
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        expires_at: 1,
    }
}

/// A daemon context over `paths` whose opener records every URL instead of starting a browser.
/// Its token and Gmail URLs are unreachable and its providers offline, so nothing reaches
/// Google.
pub(crate) fn daemon_context(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::DaemonContext,
    tokio::sync::mpsc::UnboundedReceiver<url::Url>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let opener: crate::daemon::Opener =
        std::sync::Arc::new(move |url: &url::Url, _: &crate::ops::BrowserEnv| {
            let _ = tx.send(url.clone());
            Ok(())
        });
    (offline_context(paths, opener), rx)
}

/// A provider factory for tests that never touches the network: an empty mock mailbox.
pub(crate) fn offline_factory() -> crate::sync::ProviderFactory {
    std::sync::Arc::new(|_| {
        Ok(
            std::sync::Arc::new(crate::provider::mock::MockProvider::new())
                as std::sync::Arc<dyn crate::provider::Provider>,
        )
    })
}

/// A factory handing every account `mock`.
pub(crate) fn mock_factory(
    mock: &crate::provider::mock::MockProvider,
) -> crate::sync::ProviderFactory {
    let mock = mock.clone();
    std::sync::Arc::new(move |_| {
        Ok(std::sync::Arc::new(mock.clone()) as std::sync::Arc<dyn crate::provider::Provider>)
    })
}

/// `daemon_context`, recording each launch's browser environment with its URL.
pub(crate) fn daemon_context_with_browsers(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::DaemonContext,
    tokio::sync::mpsc::UnboundedReceiver<(url::Url, crate::ops::BrowserEnv)>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let opener: crate::daemon::Opener =
        std::sync::Arc::new(move |url: &url::Url, env: &crate::ops::BrowserEnv| {
            let _ = tx.send((url.clone(), env.clone()));
            Ok(())
        });
    (offline_context(paths, opener), rx)
}

/// A context whose endpoints are unreachable and whose providers are offline.
fn offline_context(
    paths: &crate::paths::Paths,
    opener: crate::daemon::Opener,
) -> crate::daemon::DaemonContext {
    crate::daemon::DaemonContext::new(paths.clone(), opener)
        .expect("http client")
        .with_token_url(Some("http://127.0.0.1:9/token".into()))
        .with_gmail_url(Some("http://127.0.0.1:9/gmail/v1/users/me/".into()))
        .with_provider_factory(offline_factory())
}

/// Bind a real daemon on `paths` and run it on a task.
pub(crate) async fn start_daemon(
    ctx: crate::daemon::DaemonContext,
    config: crate::daemon::ServeConfig,
) -> tokio::task::JoinHandle<Result<(), crate::ops::OpError>> {
    let server = crate::daemon::Server::bind(ctx, config)
        .await
        .expect("bind the test daemon");
    tokio::spawn(server.run())
}

/// A connector whose spawn starts an in-process daemon (with the default config) and counts
/// the starts.
pub(crate) fn in_process_connector(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::Connector,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::Ordering;
    let spawned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = spawned.clone();
    let for_spawn = paths.clone();
    let spawn: crate::daemon::SpawnFn = std::sync::Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        let (ctx, _urls) = daemon_context(&for_spawn);
        tokio::spawn(async move {
            match crate::daemon::Server::bind(ctx, crate::daemon::ServeConfig::default()).await {
                Ok(server) => {
                    let _ = server.run().await;
                }
                Err(e) => panic!("test daemon failed to bind: {e}"),
            }
        });
        Ok(())
    });
    (crate::daemon::Connector::new(paths.clone(), spawn), spawned)
}

/// A stand-in for the 1Password CLI: a shell script that records every argument line in
/// `argv.log`, keeps items as files under `items/`, and fails on request.
pub(crate) struct FakeOp {
    pub path: std::path::PathBuf,
    pub state: std::path::PathBuf,
    /// Matches `Paths::all_in(dir).runtime_dir`, whose owner `OpCli` trusts.
    pub runtime_dir: std::path::PathBuf,
}

const FAKE_OP: &str = r#"#!/bin/sh
state='@STATE@'
printf '%s\n' "$*" >> "$state/argv.log"
printf '%s\n' "$0" > "$state/program"
vault=none; acct=main; prev=''
for a in "$@"; do
  [ "$prev" = --vault ] && vault=$(printf %s "$a" | tr A-Z a-z)
  [ "$prev" = --account ] && acct=$a
  prev=$a
done
dir="$state/items/$acct/$vault"
if [ -f "$state/locked" ]; then
  echo '[ERROR] 2026/10/10 08:00:00 You are not currently signed in. Please run `op signin --help` for instructions' >&2
  exit 1
fi
if [ -f "$state/hang" ]; then exec sleep 30; fi
if [ "$1 $2" = "item list" ]; then
  n=$(( $(cat "$state/lists_$acct" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$state/lists_$acct"
  if [ -f "$state/fail_list_from_$acct" ] && [ "$n" -ge "$(cat "$state/fail_list_from_$acct")" ]; then
    echo '[ERROR] 2026/10/10 08:00:00 could not list items' >&2
    exit 1
  fi
fi
case "$1" in
  whoami) echo '{"url":"my.1password.com","email":"me@example.com"}'; exit 0 ;;
  read) cat "$state/read.txt"; exit 0 ;;
esac
mkdir -p "$dir"
case "$1 $2" in
  "item list")
    printf '['; sep=''
    for f in "$dir"/*.json; do
      [ -e "$f" ] || continue
      id=$(basename "$f" .json)
      title=$(sed 's/.*"title":"\([^"]*\)".*/\1/' "$f")
      printf '%s{"id":"%s","title":"%s"}' "$sep" "$id" "$title"; sep=','
    done
    printf ']\n' ;;
  "item create")
    template=$(cat)
    if [ -f "$state/fail_on" ] && printf '%s' "$template" | grep -qF "$(cat "$state/fail_on")"; then
      echo "[ERROR] 2026/10/10 08:00:00 could not create: $template" >&2
      exit 1
    fi
    n=$(( $(cat "$state/next" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$state/next"
    printf '%s' "$template" > "$dir/item$n.json"
    printf '{"id":"item%s"}\n' "$n" ;;
  "item edit")
    [ -f "$dir/$3.json" ] || { echo "[ERROR] 2026/10/10 08:00:00 \"$3\" isn't an item." >&2; exit 1; }
    if [ -f "$state/edit_ignores_stdin" ]; then cat > /dev/null; else cat > "$dir/$3.json"; fi
    printf '{"id":"%s"}\n' "$3" ;;
  "item get")
    [ -f "$dir/$3.json" ] || { echo "[ERROR] 2026/10/10 08:00:00 \"$3\" isn't an item." >&2; exit 1; }
    sed 's/.*"fields":\(\[.*\]\)}$/\1/' "$dir/$3.json"; echo ;;
  "item delete")
    rm -f "$dir/$3.json" ;;
  *) echo "[ERROR] 2026/10/10 08:00:00 unknown command: $*" >&2; exit 1 ;;
esac
"#;

impl FakeOp {
    pub(crate) fn install(dir: &std::path::Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let state = dir.join("opstate");
        std::fs::create_dir_all(&state).unwrap();
        let path = dir.join("op");
        let script = FAKE_OP.replace("@STATE@", &state.display().to_string());
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            path,
            state,
            runtime_dir: dir.join("run"),
        }
    }

    pub(crate) fn config(&self) -> crate::config::OnePasswordConfig {
        crate::config::OnePasswordConfig {
            vault: "Rail".to_string(),
            account: None,
            op_path: Some(self.path.clone()),
        }
    }

    pub(crate) fn cli(&self) -> crate::auth::OpCli {
        crate::auth::OpCli::from_config(Some(&self.config()), &self.runtime_dir)
    }

    /// `[onepassword]` lines that point at this fake, for a hand-written config.toml.
    pub(crate) fn config_toml(&self) -> String {
        format!(
            "\n[onepassword]\nvault = \"Rail\"\nop_path = \"{}\"\n",
            self.path.display()
        )
    }

    /// Every command line `op` was run with, one per line.
    pub(crate) fn argv(&self) -> String {
        std::fs::read_to_string(self.state.join("argv.log")).unwrap_or_default()
    }

    /// The program path the fake was started as (`$0`), as the last call saw it.
    pub(crate) fn program(&self) -> String {
        std::fs::read_to_string(self.state.join("program"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub(crate) fn lock(&self) {
        std::fs::write(self.state.join("locked"), "").unwrap();
    }

    /// Make `item edit` read its template and keep the old item, answering as if it worked:
    /// what a CLI that took its edits from arguments, not stdin, would do.
    pub(crate) fn edit_ignores_stdin(&self) {
        std::fs::write(self.state.join("edit_ignores_stdin"), "").unwrap();
    }

    pub(crate) fn hang(&self) {
        std::fs::write(self.state.join("hang"), "").unwrap();
    }

    /// Fail any `item create` whose template contains `needle`, echoing the template.
    pub(crate) fn fail_on(&self, needle: &str) {
        std::fs::write(self.state.join("fail_on"), needle).unwrap();
    }

    /// Make every `item list` for `account` from its `n`th call on fail.
    pub(crate) fn fail_list_from(&self, account: &str, n: u32) {
        std::fs::write(
            self.state.join(format!("fail_list_from_{account}")),
            n.to_string(),
        )
        .unwrap();
    }

    pub(crate) fn set_read(&self, value: &str) {
        std::fs::write(self.state.join("read.txt"), value).unwrap();
    }

    /// The stored items, as the template text Rail sent.
    pub(crate) fn items(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![self.state.join("items")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(std::fs::read_to_string(path).unwrap());
                }
            }
        }
        found
    }

    /// How many items one account's vault holds (the vault name in lower case).
    pub(crate) fn item_count(&self, account: &str, vault: &str) -> usize {
        std::fs::read_dir(self.state.join("items").join(account).join(vault))
            .map_or(0, |dir| dir.flatten().count())
    }
}

/// Every file under `root` whose bytes contain `needle`, skipping `except`.
pub(crate) fn files_containing(
    root: &std::path::Path,
    needle: &str,
    except: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.starts_with(except) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
            {
                found.push(path);
            }
        }
    }
    found
}
