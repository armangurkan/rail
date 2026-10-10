# Rail

Modern email. Native to your terminal.

Rail is a Rust terminal mail client built on ratatui: a modern, fast homage to mutt with a local
store, thread view, unified inbox, readable HTML and a command palette. Gmail and Microsoft 365 first.

Status: pre-alpha. Phase 1 (config, store, login) is usable; the TUI is not built yet.

## Install

    cargo install --path crates/rail

## Setup

Rail ships no OAuth client of its own. Create one per provider once, then log in.

### Gmail / Google Workspace

1. In Google Cloud Console create a project and enable the **Gmail API**.
2. OAuth consent screen: External. **Publishing status must be "In production"**, even unverified.
   An app left in "Testing" expires refresh tokens after 7 days and Rail will keep asking you to log in.
3. Credentials: create an **OAuth client ID** of type **Desktop app**. Copy the client id and secret.
   A desktop-app secret is not actually secret; keeping it in `config.toml` is fine.
4. Log in:

       rail account add gmail --address you@gmail.com \
         --client-id XXXX.apps.googleusercontent.com --client-secret GOCSPX-...

   A browser opens for consent. Google shows an "unverified app" warning for your own app; continue.

### Microsoft 365 / Outlook

1. In Azure Portal, **App registrations** > New. Supported account types: pick what matches your
   mailbox (work/school, personal, or both). Redirect URI: platform **Mobile and desktop applications**,
   value `http://localhost` (Rail uses `http://127.0.0.1:<random port>`; Azure accepts any loopback port).
2. Under **Authentication**, enable "Allow public client flows".
3. API permissions: Microsoft Graph delegated `Mail.ReadWrite`, `Mail.Send`, `User.Read`, `offline_access`.
4. Log in:

       rail account add m365 --address you@corp.com --client-id <application-id>

   Add `--tenant <tenant-id-or-domain>` if your organisation restricts the `common` endpoint.

### Where things live

| What | Path |
|---|---|
| Config | `~/.config/rail/config.toml` |
| Store | `~/.local/share/rail/rail.db` |
| Tokens | OS keyring (service `rail`), or `~/.local/share/rail/secrets.json` with `secrets = "file"` |
| Logs | `~/.local/state/rail/rail.log` |

Override any of them with `RAIL_CONFIG_DIR`, `RAIL_DATA_DIR`, `RAIL_CACHE_DIR`, `RAIL_STATE_DIR`.

No Secret Service on your box (tty-only Linux)? Put this in `config.toml` before adding accounts:

    [general]
    secrets = "file"

### Check everything

    rail doctor
    rail account list

## Development

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    cargo deny check

Design: `docs/superpowers/specs/2026-10-09-rail-design.md`.

## License

MIT OR Apache-2.0.
