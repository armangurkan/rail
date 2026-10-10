# Changelog

All notable changes to Rail are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Two-crate Cargo workspace (`rail-core`, `rail`) on Rust 2024 with `unsafe_code = "forbid"`,
  clippy as errors, `cargo deny`, and CI for Linux and macOS.
- XDG paths with `RAIL_CONFIG_DIR`, `RAIL_DATA_DIR`, `RAIL_CACHE_DIR` and `RAIL_STATE_DIR`
  overrides.
- `config.toml` loading and validation with file path and line number in every error, plus
  comment-preserving edits.
- SQLite store with embedded migrations and the full v1 schema, including FTS5 search tables.
- Secret storage in the OS keyring, with a 0600 file fallback (`secrets = "file"`).
- OAuth2 authorization-code login with PKCE over a hardened loopback listener for Gmail and
  Microsoft 365.
- Cached, single-flight token refresh that keeps the old refresh token when the provider omits it.
- Commands: `rail account add`, `rail account list`, `rail doctor`.
- File logging to the state directory.
- Setup wizard: a full-screen ratatui wizard that adds an account from an organisation preset or
  from your own app registration, shows the portal steps, validates as you type, logs in through
  the browser and reports where everything was saved. It opens on `rail` with no config, on
  `rail setup`, and on `rail account add` without flags (or with only a provider, which it
  preselects). It exits 0 when an account was saved and 1 ("setup cancelled; nothing was saved")
  otherwise, and holds `q`, `Esc` and `Ctrl-C` while an account is being saved.
- Organisation presets: TOML files discovered in `~/.config/rail/presets/` and
  `/etc/rail/presets/`, or fetched from an `https://` URL (64 KiB cap, plain `http` refused).
- Microsoft 365 admin consent from inside the wizard, and automatic routing when a login reports
  `AADSTS65001` or `AADSTS90094`: administrators go to the consent step, everyone else gets the
  admin-consent URL to send to one.
- `rail account login <name-or-address>` to log in again and replace an account's tokens.
- The app shell the mail views will run in: terminal guard and panic hook that always restore the
  terminal, an async event loop with cancellable background tasks, and reusable widgets.

### Changed

- `rail doctor` and the token-refresh error now point at `rail account login <name>` instead of
  removing the account from `config.toml` and adding it again.
- `rail account add` flags are optional: with none (or only `gmail`/`m365`) the wizard opens. The
  flag form is unchanged for scripts, except that a partial set (`--address` without `--client-id`,
  or any other flag without `--address`) exits 1 with a message naming the missing flag.
- A missing config file now suggests `rail setup`.
- The roadmap gains Phase 2 (app shell and onboarding); sync and everything after it move back one
  phase.

### Security

- Tokens are redacted from `Debug` output and never reach logs, errors or the terminal.
- Database, secrets and config files are created owner-only and written atomically.
- The loopback listener verifies the OAuth `state` before honouring a code or an error redirect
  and escapes provider-supplied text on its error page.
- The wizard never starts when stdout is not a terminal (it prints a hint and exits 2), and nothing
  prints while it owns the screen. Client secrets are masked on screen and redacted from `Debug`
  output along with authorization codes.
- The loopback listener ignores admin-consent redirects whose `state` is absent or wrong, as it
  already did for login redirects.
