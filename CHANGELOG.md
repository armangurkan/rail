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
- A background daemon (`rail daemon run|status|stop`) that owns the store, the token stores and
  every login, started on demand and stopped after ten minutes idle. It listens on a Unix socket
  in a 0700 runtime directory (`$XDG_RUNTIME_DIR/rail`, or `RAIL_RUNTIME_DIR`), mode 0600, and
  drops connections from other users. Clients started together share one daemon, which keeps the
  environment and the `RAIL_*` directories of the client that started it and refuses clients whose
  directories differ. At most eight logins wait at once; SIGINT and SIGTERM let saves in flight
  finish. A client replaces only an older daemon; a client older than the running daemon (an open
  `rail mcp` after an upgrade) says "Rail was upgraded; restart this program" instead of stopping it.
- The operation layer: every user operation is a typed request and response (JSON-RPC 2.0, one
  message per line, 1 MiB cap) shared by the TUI, the flag commands and the MCP server.
- `rail mcp`, a stdio MCP server for AI coding agents (`claude mcp add rail -- rail mcp`) with
  twelve tools, a `setup` prompt and guided Google (`gcloud`) and Microsoft (`az`, work or school
  accounts) app registration. `rail_account_add` takes the absolute path of a client secret file,
  an `op://` reference or a preset, never a raw secret; tools refuse arguments they do not know.
  `rail_account_remove` and `rail_secrets_configure` change things only after the user confirms
  through MCP elicitation; without it nothing changes.
- `rail account remove <name-or-address>`, interactive only: it shows what goes and asks
  `Remove? [y/N]`, has no flag to skip the question, and removes nothing when stdin or stdout is
  not a terminal. The account's mail stays in the store.
- The 1Password token backend (`secrets = "1password"`, `[onepassword]`), `op://` references for
  client secrets (also in the wizard and `--client-secret`), and moving tokens between backends
  with `rail_secrets_configure`.
- The per-account `ai_send` policy (`confirm`, `drafts`, `client`), recorded and reported by
  `rail doctor` and `rail_status`; enforcement arrives with the send tools.

### Changed

- `rail doctor` and the token-refresh error now point at `rail account login <name>` instead of
  removing the account from `config.toml` and adding it again.
- `rail account add` flags are optional: with none (or only `gmail`/`m365`) the wizard opens. The
  flag form is unchanged for scripts, except that a partial set (`--address` without `--client-id`,
  or any other flag without `--address`) exits 1 with a message naming the missing flag.
- A missing config file now suggests `rail setup`.
- The roadmap gains Phase 2 (app shell and onboarding); sync and everything after it move back one
  phase.
- `rail account add`, `rail account login`, `rail account list` and `rail doctor` run through the
  daemon. `rail doctor` ends with the daemon's line and reports each account's `ai_send` policy.
- The setup wizard sends its actions to the daemon as requests; `Esc` or `Ctrl-C` cancels a login
  waiting there, and a save already begun is never interrupted.
- `rail account list` sizes its columns to the longest name and address, and `rail doctor` says
  "1 account".
- A preset file that does not parse is reported by path, line and column ("not a valid Rail
  preset") instead of the parser's message, and preset files over 64 KiB are refused.
- The Microsoft setup text names the *Application (client) ID* field, says single-tenant apps
  must give the tenant, and lists admin consent during a normal login.
- The roadmap gains Phase 3 (daemon, operations and MCP); Gmail sync and everything after it move
  back one phase.

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
- Tokens, client secrets and authorization codes never reach a daemon response, an MCP tool result,
  a log line or an `op` command line: 1Password values go through templates on `op`'s stdin and
  `op`'s errors are redacted.
- No MCP tool can choose the program Rail runs: `op_path` is set only by hand in `config.toml`, and
  Rail refuses one that is relative, not an executable file, owned by another user, or writable by
  group or others, or inside a directory that is.
- Files an agent names (a client secret JSON, a preset path) must be absolute and are read with a
  64 KiB cap; a file that does not parse is never echoed back.
- An agent can never confirm an account removal or a secret-store change for the user. The
  confirmation text is written by the daemon in plain ASCII (non-ASCII as `\u{..}`), never cut, and
  bound to a digest of the plan; if anything changed in between, nothing runs.
- Text from outside Rail (presets, provider and system error text) reaches the agent marked as
  untrusted, quoted on one line, capped at 200 characters and behind a notice that it is data; a
  provider's error text is kept apart from Rail's own message.
- `rail mcp` logs the MCP library only at error level, because its warnings can quote what an agent
  sent.
