# Contributing to Rail

Thanks for helping. Rail is early, so the bar is simple: keep it fast, keep it local-first, and
never let a secret reach a log or the terminal.

## Before you start

- Read the design specs under `docs/superpowers/specs/` (start with `2026-10-09-rail-design.md`).
  They are the authority on behaviour; the plans under `docs/superpowers/plans/` show how each phase is sequenced.
- Open an issue for anything bigger than a bug fix so the approach can be agreed first.

## Development loop

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all
    cargo deny check

All four must pass before a pull request is opened. CI runs them on Linux and macOS, checks the
minimum supported Rust version (1.88), and reports line coverage.

Write the test first. Every behaviour change ships with a test that fails without it. Tests use
temporary directories and in-process mock servers; nothing may touch the real XDG directories, the
real OS keyring, a real browser, a real 1Password account or the network. The tests that talk to the
real keyring and a real 1Password vault are `#[ignore]`d and run by hand:

    cargo test -p rail-core -- --ignored keyring_live
    RAIL_OP_LIVE_VAULT=<vault> cargo test -p rail-core -- --ignored onepassword_live
    RAIL_GMAIL_LIVE_ACCOUNT=<account name> cargo test -p rail-core -- --ignored gmail_live

The Gmail test uses your real config and token store, sends only GET requests to the Gmail API (a token refresh may POST to Google's token endpoint), and confirms the
Gmail API shapes the sync relies on. It prints a line for any check your mailbox cannot exercise
(for example no mail in the last 30 days); and if its last assertion (a very old history id gives
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
