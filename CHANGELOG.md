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
- `rail secrets set <keyring|file|1password> [--vault V] [--account A] [--no-migrate]`, the
  terminal's way to move tokens between backends (as `rail_secrets_configure` is the agent's):
  interactive only, it shows the daemon's description of the change and asks `Change? [y/N]`.
- `rail account add --store-secret-in-1password` stores the given client secret in 1Password and
  writes an `op://` reference, as `store_secret_in_1password` on `rail_account_add` does.
- `rail account remove <name-or-address>`, interactive only: it shows what goes and asks
  `Remove? [y/N]`, has no flag to skip the question, and removes nothing when stdin or stdout is
  not a terminal. The account's mail stays in the store.
- The 1Password token backend (`secrets = "1password"`, `[onepassword]`), `op://` references for
  client secrets (also in the wizard and `--client-secret`), and moving tokens between backends
  with `rail_secrets_configure`.
- The per-account `ai_send` policy (`confirm`, `drafts`, `client`), recorded and reported by
  `rail doctor` and `rail_status`; enforcement arrives with the send tools.
- Gmail sync in the daemon: labels, envelopes for `history_days` (the whole mailbox by default)
  and bodies for threads newer than `body_cache_days`, kept current every `poll_interval` by an
  always-on daemon while accounts are configured. Backfill runs newest first, so the inbox is
  usable within seconds, and resumes after a crash; an expired history cursor triggers a full
  resync, a revoked login pauses only that account, and rate limits are paced and retried. A first
  sync of a large mailbox takes hours (about 250 messages a minute, from Gmail's quota). HTML
  bodies become plain text through Rail's own linear scanner without loading anything remote;
  tables, list numbers and link addresses are not rendered, and display-quality rendering is a
  later-phase item.
- Per-account `history_days` and `body_cache_days` overrides.
- `rail sync [<account>] [--no-wait]` with progress, followed with no overall limit (it stops
  watching after 10 minutes without change, and the sync continues in the daemon), and exit codes
  0 (caught up), 1 (failed), 2 (needs a login; 2 wins over 1).
- `rail search <query> [--account <name>] [--limit N]`: the local store and Gmail's whole mailbox,
  merged by date, one escaped line per thread; results on standard output, notes on standard error.
- `rail daemon install` / `rail daemon uninstall`: start the daemon at login (systemd user unit or
  launchd agent). The unit sets the `RAIL_*` directories and a fixed `PATH`, unsets display,
  browser and 1Password variables, refuses binary paths systemd cannot run, checks the unit's daemon
  starts, and is rolled back if enabling fails. The launchd path has not been tried on a real Mac.
- `rail doctor` shows each account's sync and whether the login unit is installed, enabled and
  current, and fails for a Gmail account with no tokens or whose sync stopped on a permanent error.
- MCP tools `rail_sync_status` and `rail_sync_now`, which return no mail content.
- An ignored, read-only live test against a real Gmail account
  (`RAIL_GMAIL_LIVE_ACCOUNT=<name> cargo test -p rail-core -- --ignored gmail_live`).
- The mail screen: `rail` with an account configured opens a thread list over the local store
  (the unified inbox, one account's inbox with `1`-`9`, any label from the sidebar), keyset paged
  so the bottom of a label with thousands of threads is a keypress away, kept current from the
  daemon's progress notifications, with ASCII glyphs under `general.icons = "ascii"`.
- The reader: messages oldest first with unread ones and the latest expanded, bodies wrapped to
  the pane, quoted runs folded to `[n quoted lines]`, numbered link markers (OSC 8 hyperlinks
  where the terminal supports them) and `g x` to open one after showing its address, a read-only
  attachment list (`a`), and the pane-or-full-screen layout by `general.split_threshold`.
- `o` opens a message's original HTML in the browser from a private temporary file, after a
  once-per-session confirmation that remote images will load there.
- Filter as you type (`/`) with `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`,
  `is:starred`, `before:`, `after:` and `account:`; the command palette (`:`) with fuzzy matching
  over commands, accounts and labels; `?` help generated from the keymap; `:errors`.
- `[keys]` in `config.toml`: every key is a named action that can be rebound; bad entries are
  refused with the file and line.
- `body.fetch`, a daemon operation that fetches bodies the store does not hold yet; the reader
  shows "fetching…" meanwhile. It answers outcomes, never bodies, and is not an MCP tool.
- Link footnotes in stored bodies (`messages.links`, store version 4).

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
- The daemon stays up while any account is configured; the ten-minute idle exit now applies only
  with no accounts.
- The roadmap marks Phase 4 done; display-quality HTML rendering moves to Phase 5.
- `rail` with a config no longer exits 2 with "the TUI is not built yet": it opens the mail screen
  (or the setup wizard when no account is configured). In a pipe it exits 2 with a hint.
- Message text from HTML now carries `[n]` link markers, and plain-text bodies carry them after
  bare addresses; bodies stored before this version show no markers until they are fetched again.
- The setup wizard's keys are actions of the keymap (`wizard.*`); its behaviour is unchanged,
  except that Ctrl and Alt combinations no longer act as the plain key.
- `rail doctor` notes a `general.layout` of `split` or `stack`, which this version treats as
  `auto`.
- The roadmap marks Phase 5 done; actions and compose are next.

### Fixed

- The "Rail was upgraded" hint shows only the daemon's `MAJOR.MINOR.PATCH`, and Rail's own error
  text is escaped on the terminal.
- `rail secrets set` refuses control and format characters in `--vault`/`--account`, as MCP did.
- Account addresses refuse Unicode format characters (bidi overrides, zero-width spaces).

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
- After `op item edit` replaces an account's tokens, Rail reads the item back; if 1Password did
  not keep the new values, the save fails ("1Password did not keep the new tokens; nothing was
  saved") instead of reporting success.
- No MCP tool can choose the program Rail runs: `op_path` is set only by hand in `config.toml`, and
  Rail refuses one that is relative, not an executable file, owned by another user, or writable by
  group or others, or inside a directory that is.
- Files an agent names (a client secret JSON, a preset path) must be absolute and are read with a
  64 KiB cap; a file that does not parse is never echoed back.
- Through MCP, an account removal or a secret-store change happens only after the user confirms
  in the client's dialog; no tool input confirms for the user. An agent that can run shell
  commands could drive `rail` (or the socket) as the user, and Rail cannot stop a program running
  as the same user: the control there is the agent client's own command approval. The
  confirmation text is written by the daemon in plain ASCII (non-ASCII as `\u{..}`), never cut, and
  bound to a digest of the plan; if anything changed in between, nothing runs.
- Text from outside Rail (presets, provider and system error text) reaches the agent marked as
  untrusted, quoted on one line, capped at 200 characters and behind a notice that it is data; a
  provider's error text is kept apart from Rail's own message.
- An account's address, colour and tenant may not contain spaces or control characters, at add
  and when `config.toml` loads (with path and line), so none can carry a terminal escape.
- `store_secret_in_1password` is refused for a client secret that is already an `op://`
  reference, so Rail never copies an item a request named into its own vault.
- Preset files and client secret files are read through one guarded open: only a regular file,
  checked before and after opening, never blocking on a FIFO, at most 64 KiB.
- A daemon answer too large to send becomes an internal error for the same request instead of
  leaving the caller waiting.
- `rail mcp` logs the MCP library only at error level, because its warnings can quote what an agent
  sent.
- Mail is hostile input: header values are flattened to one line and capped, decoded words are
  capped, HTML is scanned by Rail's own linear scanner (no parser tree) that runs nothing and
  fetches nothing, and a body over 10 MiB is stored cut with a marker.
- Sync requests are pinned to the Gmail API origin, carry no mail text in URLs logged at debug
  (no query strings), and are paced under Gmail's quota with retries on rate limits.
- No MCP result carries mail content: no subject, sender, snippet or body, and Gmail's error text
  reaches agents only as outside text.
- `rail search` and sync output escape control and invisible formatting characters.
- The login unit keeps display, browser and 1Password variables out of the daemon where it can,
  and `rail daemon install` quotes paths for systemd and XML-escapes them for launchd.
- Every string from mail, Gmail or config that reaches the mail screen is escaped (controls,
  format characters, line separators), with width measured after escaping. Links are shown and
  opened only for `http`, `https` and `mailto` addresses, and only after their address is shown.
- `o` writes the HTML 0600 with a policy that blocks scripts, deletes it when the reader closes,
  and Rail itself never loads remote content.
