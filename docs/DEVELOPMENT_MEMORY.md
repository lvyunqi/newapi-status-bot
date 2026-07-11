# Development Memory

## Current State

- Repository is isolated from the QimenBot framework repository.
- Dynamic cdylib implementation, SQLite history, metrics, QQ commands, and heartbeat pushes exist.
- The release DLL and credential-free local config are installed in QimenBot ignored paths.
- Live New API and QQ integration remain intentionally incomplete.

## Recent Completion

- Added adaptive time-window batching so oversized log backlogs advance without data loss.
- Aligned model totals with visible groups while retaining unknown automatic routes.
- Added model-boundary message chunking, second-precision total latency, and initial anomaly alerts.
- Verified the API 0.3 descriptor exposes five commands and `meta/Heartbeat`.
- Passed 24 tests, strict Clippy, release build, metadata, and local DLL load checks.

## Next Step

Run one minimal read-only management-log integration after a replacement token is configured.

## Verification Baseline

- `cargo metadata --offline --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo test --offline` (24 passed)
- `cargo clippy --offline --all-targets -- -D warnings`
- `cargo build --release --offline`
- Windows `LoadLibrary` and `qimen_plugin_descriptor` export probe on the installed DLL
