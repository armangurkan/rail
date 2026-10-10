# Contributing to Rail

Thanks for helping. Rail is early, so the bar is simple: keep it fast, keep it local-first, and
never let a secret reach a log or the terminal.

## Before you start

- Read the design spec at `docs/superpowers/specs/2026-10-09-rail-design.md`. It is the authority
  on behaviour; the plans under `docs/superpowers/plans/` show how each phase is sequenced.
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
real OS keyring, or the network. The one test that talks to the real keyring is `#[ignore]`d and run
by hand:

    cargo test -p rail-core -- --ignored keyring_live

## Ground rules

- Every error is classified `Transient` or `Permanent` through a `class()` method. Nothing is
  swallowed.
- Tokens, client secrets and authorization codes never appear in logs, error messages, `Debug`
  output, or terminal lines. Redact at the type level, as `TokenSet` does.
- Files that hold mail or credentials are created owner-only (0600 files, 0700 directories) and
  written atomically.
- Source files stay under 800 lines and functions under 50. Split before you cross either.
- `unsafe` is forbidden workspace-wide.

## Commits and pull requests

Commit messages follow Conventional Commits with an optional scope:

    feat(core): cached single-flight token refresh
    fix(cli): validate the new account before opening the browser
    docs(readme): register the http://127.0.0.1 redirect URI for Azure

Keep commits focused. The pull request template asks for a summary and a test plan; fill both in.

## Licence

By contributing you agree that your work is licensed under MIT OR Apache-2.0, like the rest of Rail.
