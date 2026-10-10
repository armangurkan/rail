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
pub(crate) fn daemon_context(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::DaemonContext,
    tokio::sync::mpsc::UnboundedReceiver<url::Url>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let opener: crate::daemon::Opener = std::sync::Arc::new(move |url: &url::Url| {
        let _ = tx.send(url.clone());
        Ok(())
    });
    let ctx = crate::daemon::DaemonContext::new(paths.clone(), opener).expect("http client");
    (ctx, rx)
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
if [ -f "$state/locked" ]; then
  echo '[ERROR] 2026/10/10 08:00:00 You are not currently signed in. Please run `op signin --help` for instructions' >&2
  exit 1
fi
if [ -f "$state/hang" ]; then exec sleep 30; fi
case "$1" in
  whoami) echo '{"url":"my.1password.com","email":"me@example.com"}'; exit 0 ;;
  read) cat "$state/read.txt"; exit 0 ;;
esac
mkdir -p "$state/items"
case "$1 $2" in
  "item list")
    printf '['; sep=''
    for f in "$state"/items/*.json; do
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
    printf '%s' "$template" > "$state/items/item$n.json"
    printf '{"id":"item%s"}\n' "$n" ;;
  "item edit")
    [ -f "$state/items/$3.json" ] || { echo "[ERROR] 2026/10/10 08:00:00 \"$3\" isn't an item." >&2; exit 1; }
    cat > "$state/items/$3.json"; printf '{"id":"%s"}\n' "$3" ;;
  "item get")
    [ -f "$state/items/$3.json" ] || { echo "[ERROR] 2026/10/10 08:00:00 \"$3\" isn't an item." >&2; exit 1; }
    sed 's/.*"fields":\(\[.*\]\)}$/\1/' "$state/items/$3.json"; echo ;;
  "item delete")
    rm -f "$state/items/$3.json" ;;
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

    pub(crate) fn lock(&self) {
        std::fs::write(self.state.join("locked"), "").unwrap();
    }

    pub(crate) fn hang(&self) {
        std::fs::write(self.state.join("hang"), "").unwrap();
    }

    /// Fail any `item create` whose template contains `needle`, echoing the template.
    pub(crate) fn fail_on(&self, needle: &str) {
        std::fs::write(self.state.join("fail_on"), needle).unwrap();
    }

    pub(crate) fn set_read(&self, value: &str) {
        std::fs::write(self.state.join("read.txt"), value).unwrap();
    }

    /// The stored items, as the template text Rail sent.
    pub(crate) fn items(&self) -> Vec<String> {
        let Ok(dir) = std::fs::read_dir(self.state.join("items")) else {
            return Vec::new();
        };
        dir.map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect()
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
