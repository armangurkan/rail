# Changelog

All notable changes to Rail are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- The mail screen's frame: a header row with the scope, its counts, a search slot and one sync
  summary (`● Synced 14:47`, or the worst account's state); the sidebar, the list and the reader
  boxed at `split_threshold` (140 columns by default) and wider, the focused pane in the accent
  colour (`+`, `-` and `|` with `icons = "ascii"`); and a last row of key hints for the pane you
  are in, built from your keymap, with each account's state on the right.
  Notices (`archived 3 · z undoes`) show in place of the hints. `3 selected` and the daemon's
  state moved from the status bar to the header.
- Writing Microsoft 365 mail over Microsoft Graph: `c`, `r`, `R` and `f` write from a Microsoft
  365 account and `y` sends with the ten second `z` undo; `s` saves to Outlook's Drafts
  (`saved to Microsoft 365 · z undoes`) and later saves update the same draft, or make it again
  in Drafts when it was deleted in Outlook; Outlook drafts open in Rail with their attached files
  and `d` deletes them in Outlook. Replies and forwards are made on
  the original message, so they join the Outlook conversation; one whose original is gone from
  Outlook for good goes out as a new message. Attachments up to 25 MiB in all, through upload
  sessions from 3 MiB, each uploaded once until it changes; a forward keeps the original's
  attachments without downloading them. Rail writes plain text, so pictures inside the body are
  dropped, while attached messages and OneDrive or SharePoint links stay as Outlook has them. A
  message is never sent twice: a retry or a restart asks Outlook for its `Message-ID`, and about
  the draft Rail saved, first.
- Store schema v8: a draft keeps the provider id of the message a reply or forward answers, and
  an attachment says whether it is a plain file (Microsoft 365 attachments are listed again).
- Microsoft 365 thread actions over Microsoft Graph: `e` archives to Outlook's Archive folder,
  `d` moves to Deleted Items, `m` moves to Inbox, Archive, Junk Email or one of the account's
  folders, `s` flags the newest message or clears every flag, and `u` marks every unread message
  read or the newest one unread, each with the ten second `z` undo. Archive, trash and move leave
  Sent Items and Drafts where they are. `l` answers `Outlook uses folders, not labels: press m to
  move.`, and archive in a mailbox with no Archive folder answers `This mailbox has no Archive
  folder; press m to move.` The trash prompt names the provider's trash.
- Microsoft 365 sync and reading over Microsoft Graph: every mail folder (nested ones as
  `Parent/Child`, the well-known ones in their usual places, search and non-mail folders left
  out), threads by conversation, read and flag state, one combined delta cursor per account, new
  mail within one poll interval, `$search` server search, bodies through the shared HTML
  renderer, attachment lists, and marking read on open reaching Outlook. Labels (`l`)
  on a Microsoft 365 account are refused at once.
- The HTTP layer speaks Microsoft Graph: immutable ids on every request, Graph's error envelope,
  at most four requests in flight per mailbox, and `$batch` with throttled items sent again.
- Writing mail: `c` new message, `r` reply, `R` reply-all, `f` forward, written in your editor
  (`general.editor`, `$VISUAL`, `$EDITOR`, `vi`) and reviewed in Rail; `To:` completion from the
  addresses your mail has taught the store; attachments by path and forwarded attachments; send
  through the outbox with the 10 second `z` undo, never sent twice; local drafts kept on every
  editor round and reopened with `:drafts` or from the Drafts folder; `from_name` and `signature` per account.
- Drafts on Gmail: `s` on the review saves the draft to Gmail's Drafts after the ten second undo
  (`z` cancels it), creating the Gmail draft once and updating it after; Rail's version wins over
  an edit made elsewhere. A draft that is sent has its Gmail copy deleted; a failed delete is
  listed in `:errors`. A Gmail draft written elsewhere opens at its review, with its attachments
  fetched once into private files the daemon owns (`draft-files` in the data directory, removed
  when the draft is discarded and swept at daemon start).
- The Drafts folder: each account's `Drafts` in the sidebar lists local and Gmail drafts in one
  pane (`Enter` resumes or imports, `d` discards); `:drafts` lists the same rows and merges every
  account's in the unified inbox. Thread actions there say `Not available on drafts`.
- `draft.push` and `draft.import` operations (terminal only; the MCP tool list is unchanged).
  `OutboxDepth` gains `saving`, the drafts being saved to Gmail, shown as `saving to Gmail…`.
- Store schema v7: `drafts`, a stored `Reply-To`, outbox rows keyed by draft.
- `draft.save`, `draft.send` and `draft.discard` operations (terminal only); `action.undo` and
  `action.retry` now take the client token of the row they act on.
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
- Actions on Gmail threads from the thread list and the reader: `e` archive, `d` trash (asks
  first), `s` star, `u` read or unread, `l` labels, `m` move to a label, alone or on a selection
  (`Space`, `Esc` clears). Each applies to the store at once, can be undone with `z` for ten
  seconds, and is sent to Gmail by the daemon's per-account outbox worker, surviving restarts.
  Opening a thread marks it read. The status bar shows queued and failed actions; an action Gmail
  refuses is undone locally, shown on the thread's row and kept in `:errors`, where `Enter`
  retries it.
- Store schema v6: the outbox's scheduling columns (`not_before`, `thread_ids`, `inflight_at`,
  `reversal`).
- `action.apply`, `action.undo` and `action.retry` operations (terminal only; no MCP tool can
  change mail), the `action.failed` notification, and `outbox` counts in `sync.status`.
- Themes: `:theme` picks one of the 769 iTerm2-Color-Schemes schemes with a live preview and
  saves it as `general.theme`; `:accent` picks the accent slot (`general.accent`). Rail paints
  its own background when a theme is chosen, on truecolor terminals, and keeps the default
  look elsewhere. The free-form `[theme]` table placeholder is gone.

### Changed

- What writing says names the account's provider: `saved to Microsoft 365 · z undoes`,
  `Its copy in Outlook's Drafts is deleted too.`, `Microsoft 365 no longer has this draft`,
  `draft: saving to Microsoft 365, try again in a moment`, `s save to Microsoft 365` on the review
  and the 25 MiB refusal; Gmail's wording is unchanged. The provider trait writes one
  `OutgoingMail` (`send`, `draft_save`, `sent_already`), and Gmail's MIME is built inside its
  adapter, with the same requests on the wire.
- `Provider::apply(threads, change)` replaces `modify_threads` and `trash_threads`: a semantic
  `Change` (archive, trash, star, read, labels, move) that each adapter turns into its own calls;
  Gmail's wire calls are unchanged. `ThreadTarget`s carry each stored message's folder, flags and
  date, as they were before the action's own local change. `Profile.messages_total` is optional;
  `RemoteFolder` has a `parent`, stored in `folders.parent_id`; `SearchFailure` names the
  account's kind.
- The HTML-to-text renderer moved from `provider/gmail/html.rs` to `provider/html.rs`.
- A provider whose first sync is its delta defers the full backfill's sweep until the delta has
  caught up, and a larger `history_days` resyncs it.
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
- Plain-word search no longer matches every message for the words `name`, `address` or `null`:
  the full-text index holds the names and addresses of senders and recipients as plain text.
  Store migration 5 rebuilds the index once, the first time the daemon opens the store after the
  upgrade (this can take a moment on a large mailbox); until then the mail screen says the store
  is not set up.
- The `g x` confirmation shows its warnings first, then where the link really goes (the host, cut
  at the left so its end shows, or every `mailto:` recipient: `To`, `Cc` and `Bcc`, at most five
  lines and then "... and K more recipients"), then the address, cut if it is long. A warning
  names the whole host when the address has a user name or password before the `@`
  (`https://bank.com@evil.example`). A `mailto:` link with a parameter other than `to`, `cc`, `bcc`,
  `subject` or `body` (such as `attach=`) gets a first-line warning naming up to five of them. The
  yes/no hint always stays on the last row.
- Help and `:errors` show a `↓ more` / `↑ more` line (`v more` / `^ more` with `icons = "ascii"`)
  when there is more to scroll, and help keeps the `* set in config.toml` legend in its title;
  `:errors` scrolls with `j`/`k` or the arrow keys. Help closes on the bound help key, `Esc` or `q`.
- `rail search` retries a failing Gmail once, after a short wait (at most 2 seconds), so a Gmail
  outage fails fast; background sync keeps its retries.
- `rail sync` says "sync restarted in the daemon" when the daemon restarts its sync, and keeps
  "the daemon restarted" for a new daemon.
- The README notes that Firefox installed as a snap cannot read `o`'s files under `~/.cache` (set
  `RAIL_CACHE_DIR` to a folder it can read, or use a browser that is not a snap) and that a
  message's `<meta http-equiv="refresh">` can still move the page `o` opens.

### Fixed

- Microsoft 365 marking read touched every stored message of a thread, and one message Graph
  refused failed the whole action. Only the messages that need the change are sent, a refused one
  is skipped and logged by id, and the action fails only when every message is refused (#30).
- A 410 from Microsoft Graph outside a delta request was read as an expired delta and resynced
  the account; it is now a refusal. A throttled item inside a `$batch` waits for its own
  `Retry-After` (at most 30 seconds, one second when it names none) before it is sent again (#31, in part).
- A send or a save to Gmail whose review was closed, followed by a new message or reply before
  the daemon answered, lost its answer: no `sent · z undoes` record and no error when it failed.
  Its answer now makes its record (or shows its error) whatever is open. A thread action whose
  answer lands after a newer action now still puts a failed call's selection back and undoes its
  rows if `z` was pressed for it (saying `too late to undo` when it was); `z` still undoes only the
  newest action. While a send or save to Gmail waits for the daemon, its draft does not open from
  the Drafts folder or `:drafts`, its review refuses `e`, `a` and `x`, and a lost connection says
  `Could not confirm the send: … · check Sent and Drafts` instead of leaving `y` and `s` waiting.
- A sent imported draft's attachment files are removed when the send completes, not only at the
  next daemon start (best effort; the start sweep still catches what a removal could not).
- A send that fails on an attachment (a file that cannot be read, a forwarded attachment gone from
  Gmail or the store) no longer quotes the attachment's file name in the error, so a name from
  outside Rail is not shown unlabelled or split at `: `.
- A daemon left running from an earlier build of the same version was kept, and refused new
  operations with `no operation named "draft.push"`. Each build now carries the time `rail-core` was last built
  and sends it in the hello; a client replaces a daemon from an earlier build (or one that sends
  none), and refuses one from a later build with "restart this program". A call the daemon does
  not know now hints `rail daemon stop`, and `rail daemon status` shows the daemon's build.
- Importing a Gmail draft with attachments, and forwarding a message with its attachments, failed
  with "Invalid attachment token"; Rail now fetches an attachment by Gmail's own attachment id, an empty attachment
  imports as an empty file, and an attachment gone from Gmail says so.
- The "Rail was upgraded" hint shows only the daemon's `MAJOR.MINOR.PATCH`, and Rail's own error
  text is escaped on the terminal.
- `rail secrets set` refuses control and format characters in `--vault`/`--account`, as MCP did.
- Account addresses refuse Unicode format characters (bidi overrides, zero-width spaces).
- A label renamed in Gmail keeps the sidebar cursor, and the header keeps its unread count and shows
  the new name; a deleted label sends the screen to its account's inbox. A label beyond the
  500-per-account sidebar cap gets its own notice ("more than 500 labels") instead of "gone".
- A failed sidebar read shows in the sidebar and a failed thread read in the reader, and both reach
  the status bar and `:errors`; only a failed list read replaces the list.
- After a message fetch gets through to the daemon again, the status bar says "daemon reachable
  again" instead of keeping "daemon unreachable"; a later fetch that finds the daemon gone again
  puts "daemon unreachable" back.
- The first `j` in help and `:errors` after the top shows a new row (the `↑ more` line used to take
  the row it would have revealed).
- Upgrading a store whose full-text triggers or table were already missing no longer fails.
- An HTML answer for a message outside the open thread is dropped; the reader's cursor is clamped
  when the body shrinks; a re-read of the open thread no longer moves the focus; a refresh ends
  only when the refresh itself ends, even if it panics.
- The palette and the filter line draw the cursor where it is, so editing in the middle of the
  text shows where you are typing.
- The 500-folder sidebar cap applies per account, so one account's labels cannot hide another's.
- The terminal is restored once per session, whichever of the panic hook, the guard or a signal
  gets there first; SIGHUP and SIGTERM end Rail normally after the screen has closed, and stay
  graceful while it runs.
- `Retry-After` given as an HTTP date is honoured (capped, as a number of seconds already was; a
  date in the past retries now); concurrent requests refused with a 401 for one account share one
  token refresh, and a request makes at most three authorised attempts.
- `rail daemon install` on macOS uses the uid of the user running it, not the owner of `HOME`.
- The daemon notices a `config.toml` edit that keeps the file's modification time: it compares a
  digest of the contents as well.
- A store the daemon is still migrating is reported as a temporary error where an engine's sync
  status lines or a search meet it, instead of a failure; `rail doctor`'s store line still fails on
  any store error, and a newer store is still an error.

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
- A link is refused when its normalised address (percent-encoded, punycode) is over 2,048 bytes, not
  only when the text in the message is.
- While a box such as help or a confirmation is open, the reader draws plain `[n]` link markers,
  so a box border can never land on a hyperlink cell.
- A panicking background job or the setup wizard's job logs the length of the panic message, never
  its text, which can quote mail or a token.
