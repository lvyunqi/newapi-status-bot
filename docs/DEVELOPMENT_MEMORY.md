# Development Memory

## Current State

- Repository is isolated from the QimenBot framework repository.
- Dynamic cdylib implementation, SQLite history, metrics, QQ commands, and heartbeat pushes exist.
- The release DLL and credential-free local config are installed in QimenBot ignored paths.
- New API management authentication is live-tested; QQ integration remains incomplete.

## Recent Completion

- Added a local HTTP integration test covering probe plus multi-page model-filtered collection.
- Added retention cleanup and cursor/outcome recovery checks across SQLite reopen.
- Added request scenarios for retry success, terminal failure, and stream partial failure.
- Added a real worker start/stop test with a two-second hot-reload deadline.
- Confirmed every paginated management request carries the whitelisted `model_name`.

## Next Step

Confirm the production model whitelist and group filters in the local plugin config.

## Verification Baseline

- `cargo metadata --offline --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo test --offline` (36 passed, 1 live test ignored by default)
- focused live management-log smoke test (1 passed)
- `cargo clippy --offline --all-targets -- -D warnings`
- `cargo build --release --offline`
- Windows `LoadLibrary` and `qimen_plugin_descriptor` export probe on the installed DLL
