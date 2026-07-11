# Development Memory

## Current State

- Repository is isolated from the QimenBot framework repository.
- Dynamic cdylib implementation, SQLite history, metrics, QQ commands, and heartbeat pushes exist.
- The release DLL and credential-free local config are installed in QimenBot ignored paths.
- New API management authentication is live-tested; QQ integration remains incomplete.

## Recent Completion

- Added explicit `stale` model and group status based on the latest request timestamp.
- Kept `no_data`, `stale`, and `insufficient_samples` as separate status semantics.
- Propagated stale state into model worst-status selection and report summary counts.
- Treated stale data as an anomaly signal for confirmed heartbeat pushes.
- Added stale boundary tests while retaining collector-level freshness reporting.

## Next Step

Add HTTP pagination plus SQLite retention and restart integration tests.

## Verification Baseline

- `cargo metadata --offline --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo test --offline` (32 passed, 1 live test ignored by default)
- focused live management-log smoke test (1 passed)
- `cargo clippy --offline --all-targets -- -D warnings`
- `cargo build --release --offline`
- Windows `LoadLibrary` and `qimen_plugin_descriptor` export probe on the installed DLL
