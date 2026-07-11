# Development Memory

## Current State

- Repository is isolated from the QimenBot framework repository.
- Dynamic cdylib implementation, SQLite history, metrics, QQ commands, and heartbeat pushes exist.
- The release DLL and credential-free local config are installed in QimenBot ignored paths.
- New API management authentication is live-tested; QQ integration remains incomplete.

## Recent Completion

- Added bounded parsing for `other.admin_info.use_channel` channel ID chains.
- Added an idempotent SQLite v2 migration for sample and request-level retry chains.
- Aggregated retry chains into model metrics and exposed the top chains in `/模型异常`.
- Added migration, sanitization, request-merge, aggregation, and report-format tests.
- Kept channel keys and raw `other` payloads out of local storage.

## Next Step

Add explicit stale model/group status propagation to cached reports.

## Verification Baseline

- `cargo metadata --offline --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo test --offline` (31 passed, 1 live test ignored by default)
- focused live management-log smoke test (1 passed)
- `cargo clippy --offline --all-targets -- -D warnings`
- `cargo build --release --offline`
- Windows `LoadLibrary` and `qimen_plugin_descriptor` export probe on the installed DLL
