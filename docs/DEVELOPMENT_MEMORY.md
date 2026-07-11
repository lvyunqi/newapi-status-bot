# Development Memory

## Current State

- Repository is isolated from the QimenBot framework repository.
- Dynamic cdylib implementation, SQLite history, metrics, QQ commands, and heartbeat pushes exist.
- The release DLL and credential-free local config are installed in QimenBot ignored paths.
- New API management authentication is live-tested; QQ integration remains incomplete.

## Recent Completion

- Corrected management authentication to use the raw New API access token header.
- Added stable authentication, rate-limit, timeout, server, transport, and business error classes.
- Added tested exponential polling backoff capped at 300 seconds.
- Added a gated single-model live smoke test and passed it against the management log API.
- Persisted the valid token only in QimenBot's ignored local `.env`.

## Next Step

Parse and persist the retry channel chain from `other.admin_info.use_channel`.

## Verification Baseline

- `cargo metadata --offline --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo test --offline` (28 passed, 1 live test ignored by default)
- focused live management-log smoke test (1 passed)
- `cargo clippy --offline --all-targets -- -D warnings`
- `cargo build --release --offline`
- Windows `LoadLibrary` and `qimen_plugin_descriptor` export probe on the installed DLL
