use std::path::Path;

use crate::config::AppConfig;
use crate::metrics::{WindowSnapshot, build_snapshot};
use crate::repository::{DatabaseStats, Repository};

pub fn load_window_snapshot(
    config: &AppConfig,
    database_path: &Path,
    window_seconds: i64,
    query_at: i64,
) -> Result<WindowSnapshot, String> {
    let repository = Repository::open_read_only(database_path)?;
    let rows = repository.outcomes_since(query_at.saturating_sub(window_seconds))?;
    Ok(build_snapshot(config, &rows, query_at, window_seconds))
}

pub fn load_database_stats(database_path: &Path) -> Result<DatabaseStats, String> {
    Repository::open_read_only(database_path)?.stats()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LogSample, SampleKind};
    use tempfile::tempdir;

    #[test]
    fn live_query_uses_command_time_and_latest_committed_database_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.db");
        let mut repository = Repository::open(&path).unwrap();
        repository
            .ingest(
                &LogSample {
                    sample_key: "sample-1".to_string(),
                    source_log_id: 1,
                    request_id: "request-1".to_string(),
                    upstream_request_id: String::new(),
                    created_at: 190,
                    kind: SampleKind::Consume,
                    model_name: "echo".to_string(),
                    group_name: "default".to_string(),
                    channel_id: 1,
                    channel_name: "primary".to_string(),
                    is_stream: true,
                    prompt_tokens: 1,
                    completion_tokens: 2,
                    total_ms: Some(3_000),
                    ttft_ms: Some(500),
                    partial_failure: false,
                    attempt_index: 1,
                    retry_channel_chain: String::new(),
                    error_code: String::new(),
                    status_code: Some(200),
                    error_message: String::new(),
                },
                191,
            )
            .unwrap();

        let config = AppConfig::parse(
            r#"{"api":{"admin_user_id":3},"models":[{"name":"echo","groups":["default"]}]}"#,
        )
        .unwrap();
        let snapshot = load_window_snapshot(&config, &path, 60, 200).unwrap();

        assert_eq!(snapshot.generated_at, 200);
        assert_eq!(snapshot.window_seconds, 60);
        assert_eq!(snapshot.models[0].overall.requests, 1);
        assert_eq!(load_database_stats(&path).unwrap().outcome_count, 1);
    }
}
