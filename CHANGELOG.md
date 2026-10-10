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

### Security

- Tokens are redacted from `Debug` output and never reach logs, errors or the terminal.
- Database, secrets and config files are created owner-only and written atomically.
- The loopback listener verifies the OAuth `state` before honouring a code or an error redirect
  and escapes provider-supplied text on its error page.
