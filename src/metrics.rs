use std::collections::{BTreeMap, HashMap};

use crate::config::{AppConfig, ModelConfig};
use crate::domain::UNKNOWN_GROUP;
use crate::repository::OutcomeRow;

pub const WINDOWS: [i64; 4] = [15 * 60, 60 * 60, 24 * 60 * 60, 7 * 24 * 60 * 60];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthStatus {
    Normal,
    InsufficientSamples,
    NoData,
    Degraded,
    Abnormal,
}

impl HealthStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "正常",
            Self::InsufficientSamples => "样本不足",
            Self::NoData => "暂无样本",
            Self::Degraded => "波动",
            Self::Abnormal => "异常",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricValues {
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub partial_failures: u64,
    pub success_rate: f64,
    pub error_rate: f64,
    pub attempt_error_rate: f64,
    pub retry_rate: f64,
    pub avg_ttft_ms: Option<i64>,
    pub p50_ttft_ms: Option<i64>,
    pub p95_ttft_ms: Option<i64>,
    pub avg_total_ms: Option<i64>,
    pub p50_total_ms: Option<i64>,
    pub p95_total_ms: Option<i64>,
    pub latest_at: i64,
    pub error_codes: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct GroupReport {
    pub group_name: String,
    pub status: HealthStatus,
    pub metrics: MetricValues,
}

#[derive(Debug, Clone)]
pub struct ModelReport {
    pub model_name: String,
    pub display_name: String,
    pub status: HealthStatus,
    pub overall: MetricValues,
    pub groups: Vec<GroupReport>,
}

#[derive(Debug, Clone, Default)]
pub struct WindowSnapshot {
    pub generated_at: i64,
    pub window_seconds: i64,
    pub models: Vec<ModelReport>,
}

#[derive(Debug, Default)]
struct Accumulator {
    rows: Vec<OutcomeRow>,
}

impl Accumulator {
    fn finish(self) -> MetricValues {
        let requests = self.rows.len() as u64;
        let successes = self
            .rows
            .iter()
            .filter(|row| row.outcome == "success")
            .count() as u64;
        let partial_failures = self
            .rows
            .iter()
            .filter(|row| row.outcome == "partial_failed")
            .count() as u64;
        let failures = requests.saturating_sub(successes + partial_failures);
        let attempts = self
            .rows
            .iter()
            .map(|row| row.attempt_count.max(0) as u64)
            .sum::<u64>();
        let errors = self
            .rows
            .iter()
            .map(|row| row.error_count.max(0) as u64)
            .sum::<u64>();
        let retries = self.rows.iter().filter(|row| row.attempt_count > 1).count() as u64;
        let ttft = self
            .rows
            .iter()
            .filter_map(|row| row.ttft_ms)
            .collect::<Vec<_>>();
        let totals = self
            .rows
            .iter()
            .filter_map(|row| row.total_ms)
            .collect::<Vec<_>>();
        let mut error_codes = HashMap::<String, u64>::new();
        for row in &self.rows {
            if row.outcome != "success" {
                let code = if row.latest_error_code.is_empty() {
                    "unknown_error"
                } else {
                    row.latest_error_code.as_str()
                };
                *error_codes.entry(code.to_string()).or_default() += 1;
            }
        }
        let mut error_codes = error_codes.into_iter().collect::<Vec<_>>();
        error_codes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        MetricValues {
            requests,
            successes,
            failures,
            partial_failures,
            success_rate: percentage(successes, requests),
            error_rate: percentage(failures + partial_failures, requests),
            attempt_error_rate: percentage(errors, attempts),
            retry_rate: percentage(retries, requests),
            avg_ttft_ms: average(&ttft),
            p50_ttft_ms: percentile(&ttft, 0.50),
            p95_ttft_ms: percentile(&ttft, 0.95),
            avg_total_ms: average(&totals),
            p50_total_ms: percentile(&totals, 0.50),
            p95_total_ms: percentile(&totals, 0.95),
            latest_at: self.rows.iter().map(|row| row.last_seen).max().unwrap_or(0),
            error_codes,
        }
    }
}

pub fn build_snapshot(
    config: &AppConfig,
    rows: &[OutcomeRow],
    generated_at: i64,
    window_seconds: i64,
) -> WindowSnapshot {
    let cutoff = generated_at - window_seconds;
    let mut reports = Vec::with_capacity(config.models.len());
    for model in &config.models {
        let model_rows = rows
            .iter()
            .filter(|row| {
                row.model_name == model.name
                    && row.last_seen >= cutoff
                    && (model.groups.is_empty()
                        || model
                            .groups
                            .iter()
                            .any(|allowed| allowed == &row.group_name)
                        || row.group_name == UNKNOWN_GROUP)
            })
            .cloned()
            .collect::<Vec<_>>();
        let overall = Accumulator {
            rows: model_rows.clone(),
        }
        .finish();
        let mut by_group: BTreeMap<String, Vec<OutcomeRow>> = BTreeMap::new();
        for row in model_rows {
            by_group
                .entry(row.group_name.clone())
                .or_default()
                .push(row);
        }
        for configured in &model.groups {
            by_group.entry(configured.clone()).or_default();
        }
        let groups = by_group
            .into_iter()
            .map(|(group_name, rows)| {
                let metrics = Accumulator { rows }.finish();
                let status = classify(config, model, &metrics);
                GroupReport {
                    group_name,
                    status,
                    metrics,
                }
            })
            .collect::<Vec<_>>();
        let status = groups
            .iter()
            .map(|group| group.status)
            .max()
            .unwrap_or_else(|| classify(config, model, &overall));
        reports.push(ModelReport {
            model_name: model.name.clone(),
            display_name: if model.display_name.is_empty() {
                model.name.clone()
            } else {
                model.display_name.clone()
            },
            status,
            overall,
            groups,
        });
    }
    WindowSnapshot {
        generated_at,
        window_seconds,
        models: reports,
    }
}

fn classify(config: &AppConfig, model: &ModelConfig, metrics: &MetricValues) -> HealthStatus {
    if metrics.requests == 0 {
        return HealthStatus::NoData;
    }
    if metrics.requests < config.status.minimum_samples {
        return HealthStatus::InsufficientSamples;
    }
    if metrics.success_rate < config.status.degraded_success_rate {
        return HealthStatus::Abnormal;
    }
    let max_ttft = model
        .max_ttft_ms
        .unwrap_or(config.status.default_max_ttft_ms);
    let max_total = model
        .max_total_ms
        .unwrap_or(config.status.default_max_total_ms);
    let latency_degraded = metrics.p95_ttft_ms.is_some_and(|value| value > max_ttft)
        || metrics.p95_total_ms.is_some_and(|value| value > max_total);
    if metrics.success_rate < config.status.normal_success_rate || latency_degraded {
        HealthStatus::Degraded
    } else {
        HealthStatus::Normal
    }
}

fn percentage(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn average(values: &[i64]) -> Option<i64> {
    (!values.is_empty()).then(|| values.iter().sum::<i64>() / values.len() as i64)
}

fn percentile(values: &[i64], quantile: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted.get(rank).copied()
}

pub fn snapshot_map(
    config: &AppConfig,
    rows: &[OutcomeRow],
    now: i64,
) -> HashMap<i64, WindowSnapshot> {
    WINDOWS
        .into_iter()
        .map(|window| (window, build_snapshot(config, rows, now, window)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AppConfig {
        serde_json::from_str::<AppConfig>(
            r#"{"api":{"admin_user_id":3},"models":[{"name":"echo","groups":["default"]}]}"#,
        )
        .unwrap()
    }

    fn row(outcome: &str, attempts: i64, errors: i64, ttft: i64) -> OutcomeRow {
        OutcomeRow {
            model_name: "echo".to_string(),
            group_name: "default".to_string(),
            outcome: outcome.to_string(),
            last_seen: 1000,
            total_ms: Some(3000),
            ttft_ms: Some(ttft),
            attempt_count: attempts,
            error_count: errors,
            latest_error_code: String::new(),
        }
    }

    #[test]
    fn calculates_request_and_attempt_rates() {
        let rows = vec![row("success", 2, 1, 100), row("failed", 2, 2, 300)];
        let snapshot = build_snapshot(&config(), &rows, 1000, 900);
        let metrics = &snapshot.models[0].overall;
        assert_eq!(metrics.success_rate, 50.0);
        assert_eq!(metrics.attempt_error_rate, 75.0);
        assert_eq!(metrics.retry_rate, 100.0);
        assert_eq!(metrics.p50_ttft_ms, Some(300));
    }

    #[test]
    fn keeps_configured_group_without_samples() {
        let snapshot = build_snapshot(&config(), &[], 1000, 900);
        assert_eq!(snapshot.models[0].groups[0].status, HealthStatus::NoData);
    }

    #[test]
    fn keeps_unknown_route_but_excludes_unlisted_groups_from_overall() {
        let mut unknown = row("failed", 1, 1, 100);
        unknown.group_name = UNKNOWN_GROUP.to_string();
        let mut hidden = row("success", 1, 0, 100);
        hidden.group_name = "not-configured".to_string();

        let snapshot = build_snapshot(&config(), &[unknown, hidden], 1000, 900);
        let model = &snapshot.models[0];
        assert_eq!(model.overall.requests, 1);
        assert!(
            model
                .groups
                .iter()
                .any(|group| group.group_name == UNKNOWN_GROUP)
        );
        assert!(
            !model
                .groups
                .iter()
                .any(|group| group.group_name == "not-configured")
        );
    }
}
