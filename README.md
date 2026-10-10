# Rail

**Modern email. Native to your terminal.**

[![ci](https://github.com/vitosans/rail/actions/workflows/ci.yml/badge.svg)](https://github.com/vitosans/rail/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
![rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

Rail is a Rust terminal mail client built on ratatui: a modern, fast homage to mutt with a local
store, a thread view, a unified inbox, readable HTML and a command palette. Gmail and Microsoft 365
first.

**Status: pre-alpha.** Phase 1 (config, store, login, doctor) is usable from the command line. The
TUI is not built yet.

## Why Rail

- **Threads are the unit.** The list shows conversations, and the reader collapses and expands them.
- **One inbox.** All accounts merge into a unified view with per-account colour and a quick filter.
- **Readable HTML.** HTML mail renders in the terminal, with one key to open it in a browser.
- **Everything is local.** Every list, scroll, search and open is a SQLite query. The UI never waits
  on the network.
- **Palette and keymap.** `:` fuzzy-matches every command, folder and account; `?` shows the keymap for
  the current view.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 1 | Workspace, config, store schema, secrets, OAuth login, `rail account`, `rail doctor` | Done |
| 2 | Gmail provider, sync engine, outbox, writer actor and read pool: `rail sync` fills the store | Next |
| 3 | TUI read path: thread list, sidebar, reader, palette, help, filter | Planned |
| 4 | Actions through the outbox and the compose round-trip | Planned |
| 5 | Microsoft Graph provider and device-code flow | Planned |
| 6 | Polish: search operators, auto layout, theme, inline images | Planned |

The full design is in [`docs/superpowers/specs/2026-10-09-rail-design.md`](docs/superpowers/specs/2026-10-09-rail-design.md).

## Install

Requires Rust 1.88 or newer.

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
   value `http://127.0.0.1`. Rail listens on `http://127.0.0.1:<random port>`, and Azure matches any
   port for a loopback redirect. If the portal refuses an `http://127.0.0.1` URI, add it by editing the
   app's **Manifest** (`replyUrlsWithType`) instead.
2. Under **Authentication**, enable "Allow public client flows".
3. API permissions: Microsoft Graph delegated `Mail.ReadWrite`, `Mail.Send`, `User.Read`, `offline_access`.
4. Log in:

       rail account add m365 --address you@corp.com --client-id <application-id>

   Add `--tenant <tenant-id-or-domain>` if your organisation restricts the `common` endpoint.

### Check everything

    rail doctor
    rail account list

`rail doctor` reports the config file, the store and its schema version, the secrets backend, and
token presence for every account. It exits non-zero when anything needs attention.

## Configuration

`rail account add` writes `config.toml` for you and preserves any comments you add later. A minimal
file looks like this:

```toml
[general]
# "keyring" (default) stores tokens in the OS keyring; "file" uses a private file instead.
secrets = "keyring"

[[accounts]]
name = "work"
kind = "gmail"            # "gmail" or "m365"
address = "you@example.com"
client_id = "XXXX.apps.googleusercontent.com"
client_secret = "GOCSPX-..."   # Gmail only
color = "blue"
```

Tokens are never written to `config.toml`.

### Where things live

| What | Linux path |
|---|---|
| Config | `~/.config/rail/config.toml` |
| Store | `~/.local/share/rail/rail.db` |
| Tokens | OS keyring (service `rail`), or `~/.local/share/rail/secrets.json` with `secrets = "file"` |
| Logs | `~/.local/state/rail/rail.log` |

macOS uses the platform directories under `~/Library/Application Support/rail` instead. Override any
location with `RAIL_CONFIG_DIR`, `RAIL_DATA_DIR`, `RAIL_CACHE_DIR` and `RAIL_STATE_DIR`.

No Secret Service on your box (tty-only Linux)? Put this in `config.toml` before adding accounts:

```toml
[general]
secrets = "file"
```

Files that hold mail or credentials are created owner-only and written atomically.

## Project layout

```
crates/rail-core/        Library: paths, config, store, auth. No terminal dependency.
  src/config/            TOML model, loading with line-numbered errors, comment-preserving edits
  src/store/             SQLite open, migrations, accounts repository
  src/auth/              Secret stores, OAuth endpoints, loopback listener, login flow, token refresh
crates/rail/             Binary: clap CLI, logging, `account` and `doctor` commands
  tests/cli.rs           End-to-end tests against the built binary in a temporary XDG layout
docs/superpowers/        Design spec and phase plans
deny.toml                Licence and advisory policy for `cargo deny`
```

## Development

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    cargo deny check

Line coverage (needs `cargo-llvm-cov`):

    cargo llvm-cov --workspace

Tests never touch the real XDG directories, the real OS keyring, or the network. The one test that
exercises the real keyring is ignored by default:

    cargo test -p rail-core -- --ignored keyring_live

See [CONTRIBUTING.md](CONTRIBUTING.md) for the ground rules and commit format, and
[CHANGELOG.md](CHANGELOG.md) for what has shipped.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
