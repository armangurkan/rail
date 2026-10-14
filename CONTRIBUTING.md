# Contributing to Rail

Thanks for helping. Rail is early, so the bar is simple: keep it fast, keep it local-first, and
never let a secret reach a log or the terminal.

## Before you start

- Read the [README](README.md) first: it describes how Rail behaves today, and it is kept in step
  with the binary. Where the code and the README disagree, that is a bug in one of them.
- Open an issue for anything bigger than a bug fix so the approach can be agreed first.

## Project layout

```
crates/rail-core/        Library: paths, config, store, auth, onboarding, operations, daemon, sync. No terminal dependency.
  src/config/            TOML model, loading with line-numbered errors, comment-preserving edits
  src/store/             SQLite open, migrations, repositories
    store/view.rs        Read-only queries the mail screen runs: thread pages (an inbox, a folder, Starred, Sent and Trash across accounts), the sidebar's counts, one thread, a message's HTML
    store/filter.rs      The filter line's syntax as SQL conditions
    store/outbox.rs      The outbox queue the action operations and the worker share
    store/drafts.rs      Local drafts, their recipients and attachments
    store/drafts_folder.rs The Drafts folder's rows (local and provider), their count, and the import's source query
    store/outbox_drafts.rs The outbox rows that save a draft to the provider and delete its provider copy
    store/addresses.rs   The addresses mail has taught the store, for `To:` completion
    store/apply_local.rs Local changes of actions and their exact reverse, and the conflict rule
  src/auth/              Secret stores (keyring, file, 1Password), OAuth endpoints, loopback listener, login and admin consent, token refresh
  src/onboarding/        Presets and the setup wizard's pure state machine
  src/ops/               The operation layer: every request and response type, OpError, Untrusted
    ops/drafts.rs        The draft operations' types and the limits on a message
  src/daemon/            JSON-RPC wire format, client and connector, the socket server and its handlers
    daemon/handlers/drafts.rs  The `draft.save`, `draft.send` and `draft.discard` handlers
    daemon/handlers/drafts_gmail.rs  The `draft.push` and `draft.import` handlers (either provider)
    daemon/handlers/draft_files.rs  The private files of an imported draft: fetch, removal, the start-up sweep
  src/accounts/          The add, login and remove pipeline the daemon runs
  src/guide/             Google and Microsoft setup steps as versioned data
  src/provider/          The provider trait, the shared HTTP layer (quota pacing, retries, the Graph dialect and limiter), the HTML-to-text scanner (`provider/html.rs`), and the Gmail and Graph providers
    provider/graph/      Microsoft 365 over Graph: folders, the combined delta cursor, $batch, search, bodies, mark read, and writing through Outlook drafts
    provider/graph/drafts.rs, upload.rs, send.rs  Outlook drafts, attachment upload sessions, and sending a draft once
    provider/graph/reconcile.rs  Matching an Outlook draft's file attachments to Rail's: keep, delete, upload
    provider/http_upload.rs  One upload-session chunk: a `PUT` with no `Authorization`, its address never logged
    provider/gmail/write.rs  Gmail's writing entry point: an `OutgoingMail` built with the MIME builder, then sent or saved
    provider/gmail/mime.rs, send.rs  The MIME builder (RFC 5322 bytes), and Gmail's raw send and "sent already" lookup
    provider/gmail/drafts.rs  Gmail's drafts: create, update, delete, find
    provider/mock/       The in-memory provider the tests use (a module: `mod.rs`, `drafts.rs`, `tests.rs`)
  src/sync/              Per-account sync engines, backfill and delta passes (a delta-first provider's deferred sweep), the single store writer, search
    sync/body_fetch.rs   Fetches bodies the store does not hold yet (the `body.fetch` operation)
    sync/outbox/         The per-account outbox worker that sends actions to the account's provider
    sync/outbox/compose.rs Assembles a draft into the `OutgoingMail` the provider sends
    sync/outbox/pushing.rs Saves a draft to the provider (create or update) and deletes its provider copy
  src/doctor.rs          The `rail doctor` checks
crates/rail/             Binary: clap CLI, logging, and the account, daemon, doctor, mcp, search, secrets, setup and sync commands
  src/cmd/mcp/           The MCP server: tools, the setup prompt, confirmation through elicitation, summaries
  src/tui/               App shell: terminal guard, event loop, widgets, the wizard screen and its effects
    tui/keys             Actions, their default keys and `[keys]` overrides: the keymap every screen reads
    tui/text             Escaping, width, wrapping and hyperlinks for text that came from outside
    tui/data             The mail screen's read side (the store reader) and its link to the daemon
    tui/screens/mail     The mail screen: thread list, sidebar, status bar; `events.rs` takes the store's and the daemon's answers, `header.rs` and `hints.rs` draw the header row and the hint bar, `sidebar_tree.rs` builds the sidebar's rows and folds and `sidebar_view.rs` draws them, `composing*.rs` is its side of writing mail, `drafts_pane.rs` and `drafts_flow.rs` its Drafts folder
    tui/screens/mail/tests/compose_gmail.rs, drafts_folder.rs  The tests of saving to Gmail and of the Drafts folder
    tui/screens/reader   The reader: messages, folded quotes, numbered links
    tui/screens/compose  The composer: the compose file, reply and forward prefill, prompts with completion, the review
    tui/editor.rs        Which editor to run and how (`handover.rs` hands the terminal over)
    tui/modals           The palette, filter line, help, confirmations, label pickers and the error list
    tui/theme            The default look, the vendored scheme table and the mapping from a scheme (scripts/update-schemes.sh regenerates schemes.txt; never edit it by hand)
  tests/                 End-to-end tests against the built binary, in a pseudo-terminal, and over MCP
assets/brand/            Logos and the app icon
deny.toml                Licence and advisory policy for `cargo deny`
scripts/                 update-schemes.sh: regenerates the vendored colour-scheme table from a pinned commit of iTerm2-Color-Schemes
```

## Development loop

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all
    cargo deny check

Always run tests as `cargo test --workspace`, with any filter after `--`: for example
`cargo test --workspace -- compose::` runs the composer's tests and renders its review
snapshots. `-p` builds `rail-core` with a different feature set (`rail` turns on its
`test-gates` feature), so mixing `-p` and `--workspace` keeps two builds of every crate in
`target/`. Debug builds carry line tables for Rail's crates and no debug info for dependencies
(`[profile.dev]` in `Cargo.toml`); to step through code with gdb or lldb, set `debug = true`
there locally. `cargo clean` (or `cargo sweep --time 2` from `cargo-sweep`) clears old build
output, which Cargo never removes on its own.

Run tests in the default (debug) profile. Under `cargo test --release` the test URL overrides
(`with_graph_url`, `with_gmail_url`, `with_token_url`) are compiled out, so tests that rely on them
cannot work.

All four must pass before a pull request is opened. CI runs them on Linux and macOS, checks the
minimum supported Rust version (1.88), and reports line coverage.

Line coverage needs `cargo-llvm-cov`:

    cargo llvm-cov --workspace

Write the test first. Every behaviour change ships with a test that fails without it. Tests use
temporary directories and in-process mock servers; nothing may touch the real XDG directories, the
real OS keyring, a real browser, a real 1Password account (a fake `op` script stands in), a real
systemd or launchd unit, or the network.

### Snapshot tests

Screens are checked with [insta](https://insta.rs) snapshots of a `TestBackend` frame: the
`.snap` files live in a `snapshots/` directory next to the test module and are committed.

- A new snapshot: `INSTA_UPDATE=unseen cargo test --workspace -- <test>` writes it; read it before
  committing.
- A change you meant: `cargo test` fails and writes `<name>.snap.new` beside the old file. Review
  with `cargo insta review` (from `cargo install cargo-insta`), or compare the two files and
  rename the `.snap.new` over the `.snap`.
- CI never writes snapshots (insta sees `CI`): a missing or changed snapshot fails the build.
- Snapshot rows are trimmed of trailing spaces, and every clock in a snapshot test is fixed, in
  UTC; hyperlinks are off.

### Test hooks

Debug builds honour a few `RAIL_TEST_*` environment variables so the binary tests can stand in for
the browser (`RAIL_TEST_OPENER`), the token endpoint (`RAIL_TEST_TOKEN_URL`), the Gmail API root
(`RAIL_TEST_GMAIL_URL`), shorten the daemon's idle timeout (`RAIL_TEST_IDLE_SECS`,
`RAIL_TEST_IDLE_WITH_ACCOUNTS`) and `rail sync`'s stall watch (`RAIL_TEST_SYNC_STALL_SECS`), make
the mail screen panic on its first key (`RAIL_TEST_MAIL_PANIC`, for the terminal-restore test),
and record the `systemctl`/`launchctl` commands `rail daemon install` would run instead of running them
(`RAIL_TEST_SERVICE_LOG`). Release builds ignore them all. Every binary test also sets
`XDG_CONFIG_HOME` (and, on macOS, `HOME`) inside its temporary directory. One unit test checks the
generated systemd unit with `systemd-analyze verify` and is skipped where that is missing; the
pseudo-terminal tests use util-linux `script` and are skipped where it is missing.

`crates/rail-core/src/guide/tests.rs` checks that the README's setup sections carry the current
guide marker (`<!-- guide:google v… -->`, `<!-- guide:microsoft v… -->`) and every guide command
verbatim. Change the guide and the README together (`cargo test --workspace -- guide::`).

### Live tests

The tests that talk to the real keyring, a real 1Password vault and the real Gmail API are
`#[ignore]`d and run by hand:

    cargo test --workspace -- --ignored keyring_live
    RAIL_OP_LIVE_VAULT=<vault> cargo test --workspace -- --ignored onepassword_live
    RAIL_GMAIL_LIVE_ACCOUNT=<account name> cargo test --workspace -- --ignored gmail_live

The Gmail test uses your real config and token store, sends only GET requests to the Gmail API (a
token refresh may POST to Google's token endpoint), prints counts and never mail, and confirms the
Gmail API shapes the sync relies on. It prints a line for any check your mailbox cannot exercise
(for example no mail in the last 30 days). If its last assertion (a very old history id gives
`CursorExpired`) fails, Google's behaviour differs from the assumption, which needs revisiting;
that does not by itself mean Rail is broken.

## Ground rules

- Every error is classified `Transient` or `Permanent` through a `class()` method. Nothing is
  swallowed.
- Tokens, client secrets and authorization codes never appear in logs, error messages, `Debug`
  output, or terminal lines. Redact at the type level, as `TokenSet` does.
- Files that hold mail or credentials are created owner-only (0600 files, 0700 directories) and
  written atomically.
- Source files stay under 800 lines and functions under 50. Split before you cross either.
- `unsafe` is forbidden workspace-wide.
- The README tracks behaviour. Any change a user can see or type (a command, a flag, a config key,
  an environment variable, an output line, an MCP tool) updates `README.md` in the same change, and
  reviewers check the README against the binary.
- Every user operation is an operation in `rail-core::ops` that the daemon runs; the TUI, the flag
  commands and `rail mcp` only send requests. No capability exists only in one front end.
- Store migrations are embedded and numbered; never edit a shipped migration's statements, add a
  new one instead.

## Commits and pull requests

Commit messages follow Conventional Commits with an optional scope:

    feat(core): cached single-flight token refresh
    fix(cli): validate the new account before opening the browser
    docs(readme): register the http://127.0.0.1 redirect URI for Azure

Keep commits focused. The pull request template asks for a summary and a test plan; fill both in.
Commit messages and pull request descriptions carry no tool or AI attribution: no `Co-Authored-By`
trailer, no "Generated with" line and no session links.

## Licence

By contributing you agree that your work is licensed under MIT OR Apache-2.0, like the rest of Rail.
