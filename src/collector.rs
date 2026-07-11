use std::sync::Arc;
use std::time::Duration;

use crate::api::NewApiClient;
use crate::domain::LogSample;
use crate::metrics::snapshot_map;
use crate::repository::Repository;
use crate::state::{AppState, ReportCache, unix_now};

/// 后台线程入口。所有日志网络访问和 SQLite 写入均保持在该线程内。
pub fn run(state: Arc<AppState>) {
    let mut repository = match Repository::open(&state.database_path) {
        Ok(repository) => repository,
        Err(error) => {
            update_failure(&state, error);
            return;
        }
    };
    let mut last_cleanup_at = 0;
    loop {
        let now = unix_now();
        if let Ok(mut health) = state.health.write() {
            health.last_attempt_at = now;
        }

        let cycle_result = collect_cycle(&state, &mut repository, now);
        let cycle_ok = cycle_result.is_ok();
        match cycle_result {
            Ok(()) => update_success(&state, now),
            Err(error) => update_failure(&state, error),
        }

        if now - last_cleanup_at >= 24 * 60 * 60 {
            let retention = i64::from(state.config.storage.retention_days) * 24 * 60 * 60;
            if let Err(error) = repository.cleanup(now - retention) {
                eprintln!("[newapi-status-bot] cleanup failed: {error}");
            } else {
                last_cleanup_at = now;
            }
        }

        let failures = state
            .health
            .read()
            .map(|health| health.consecutive_failures)
            .unwrap_or(1);
        let multiplier = 1_u64 << failures.min(4);
        let wait_seconds = if cycle_ok {
            state.config.api.poll_interval_secs
        } else {
            state
                .config
                .api
                .poll_interval_secs
                .saturating_mul(multiplier)
                .min(300)
        };
        if state.control.wait(Duration::from_secs(wait_seconds)) {
            break;
        }
    }
}

fn collect_cycle(
    state: &Arc<AppState>,
    repository: &mut Repository,
    now: i64,
) -> Result<(), String> {
    let client = NewApiClient::new(&state.config.api).map_err(|error| error.to_string())?;
    let mut failures = Vec::new();
    let mut completed_models = 0_usize;

    for model in &state.config.models {
        match collect_model(state, repository, &client, &model.name, now) {
            Ok(()) => completed_models += 1,
            Err(error) => {
                let _ = repository.record_failure(&model.name, &error);
                failures.push(format!("{}: {error}", model.name));
            }
        }
    }

    repository.settle_pending(now - state.config.api.settlement_grace_secs)?;
    let retention = i64::from(state.config.storage.retention_days) * 24 * 60 * 60;
    let outcomes = repository.outcomes_since(now - retention)?;
    let reports = snapshot_map(&state.config, &outcomes, now);
    let database = repository.stats()?;
    if let Ok(mut cache) = state.reports.write() {
        *cache = ReportCache {
            generated_at: now,
            windows: reports,
            database,
        };
    }

    if completed_models == 0 {
        return Err(if failures.is_empty() {
            "没有可采集的白名单模型".to_string()
        } else {
            failures.join("; ")
        });
    }
    if !failures.is_empty() {
        eprintln!(
            "[newapi-status-bot] partial collection failure: {}",
            failures.join("; ")
        );
    }
    Ok(())
}

fn collect_model(
    state: &Arc<AppState>,
    repository: &mut Repository,
    client: &NewApiClient,
    model: &str,
    now: i64,
) -> Result<(), String> {
    let cursor = repository.cursor(model)?;
    let start = if cursor == 0 {
        now - state.config.api.initial_backfill_hours as i64 * 60 * 60
    } else {
        cursor.saturating_sub(state.config.api.overlap_secs)
    }
    .min(now.saturating_sub(1));
    let capacity = i64::from(state.config.api.max_pages_per_model)
        .saturating_mul(i64::from(state.config.api.page_size));
    let batch_end = select_batch_end(start, now, capacity, |end| {
        client
            .fetch_logs(model, start, end, 1, state.config.api.page_size)
            .map(|page| page.total)
            .map_err(|error| error.to_string())
    })?;
    let mut page_number = 1_u32;
    let mut remote_logs = Vec::new();
    let mut complete = false;

    while page_number <= state.config.api.max_pages_per_model {
        let page = client
            .fetch_logs(
                model,
                start,
                batch_end,
                page_number,
                state.config.api.page_size,
            )
            .map_err(|error| error.to_string())?;
        let item_count = page.items.len();
        remote_logs.extend(page.items);
        let consumed = i64::from(page_number) * i64::from(state.config.api.page_size);
        if item_count < state.config.api.page_size as usize || consumed >= page.total {
            complete = true;
            break;
        }
        page_number += 1;
    }
    if !complete {
        return Err(format!(
            "时间窗口日志超过 {} 页，未推进游标以避免丢失",
            state.config.api.max_pages_per_model
        ));
    }

    remote_logs.sort_by_key(|log| (log.created_at, log.id));
    for remote in remote_logs {
        if remote.model_name != model {
            continue;
        }
        if let Some(sample) = LogSample::from_remote(remote) {
            repository.ingest(&sample, now)?;
        }
    }
    repository.record_success(model, batch_end, now)
}

/// 找到本轮可完整分页的最大时间边界，确保大积压会逐轮推进而不是原地重试。
fn select_batch_end<F>(
    start: i64,
    requested_end: i64,
    capacity: i64,
    mut count_until: F,
) -> Result<i64, String>
where
    F: FnMut(i64) -> Result<i64, String>,
{
    if capacity <= 0 || requested_end <= start {
        return Err("采集时间窗口或分页容量无效".to_string());
    }
    if count_until(requested_end)? <= capacity {
        return Ok(requested_end);
    }

    let mut low = start.saturating_add(1);
    let mut high = requested_end.saturating_sub(1);
    let mut selected = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        if count_until(middle)? <= capacity {
            selected = Some(middle);
            low = middle.saturating_add(1);
        } else {
            high = middle.saturating_sub(1);
        }
    }
    selected.ok_or_else(|| format!("单个时间戳内日志超过分页容量 {capacity}，无法无损推进游标"))
}

fn update_success(state: &AppState, now: i64) {
    if let Ok(mut health) = state.health.write() {
        health.last_success_at = now;
        health.consecutive_failures = 0;
        health.last_error.clear();
    }
}

fn update_failure(state: &AppState, error: String) {
    if let Ok(mut health) = state.health.write() {
        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        health.last_error = error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrows_oversized_backlog_to_largest_complete_window() {
        let end = select_batch_end(100, 200, 250, |candidate| {
            Ok((candidate - 100).saturating_mul(10))
        })
        .unwrap();
        assert_eq!(end, 125);
    }

    #[test]
    fn rejects_single_timestamp_over_capacity() {
        let result = select_batch_end(100, 200, 50, |_| Ok(51));
        assert!(result.is_err());
    }
}
