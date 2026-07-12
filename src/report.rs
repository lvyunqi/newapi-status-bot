use crate::api::PerfMetricData;
use crate::config::AppConfig;
use crate::metrics::{HealthStatus, ModelReport, WindowSnapshot};
use crate::state::{CollectorHealth, ReportCache};

pub fn format_status(
    config: &AppConfig,
    cache: &ReportCache,
    health: &CollectorHealth,
    window: i64,
    model_query: Option<&str>,
) -> Result<Vec<String>, String> {
    let snapshot = cache
        .windows
        .get(&window)
        .ok_or_else(|| "统计快照尚未生成，请稍后重试".to_string())?;
    let models = select_models(snapshot, model_query)?;
    let stale = health.last_success_at == 0
        || snapshot.generated_at - health.last_success_at > config.status.stale_after_secs;
    let counts = status_counts(&models);
    let collector_status = if stale {
        "⏳数据过期"
    } else {
        "✅正常"
    };
    let mut header = format!(
        "📊 模型状态｜{}\n🕒 {}｜采集 {}\n白名单 {}｜✅{} ⚠️{} ❌{} ⏳{} ◻️{}",
        window_label(snapshot.window_seconds),
        format_timestamp(snapshot.generated_at),
        collector_status,
        models.len(),
        counts.normal,
        counts.degraded,
        counts.abnormal,
        counts.stale,
        counts.no_data,
    );
    if stale && !health.last_error.is_empty() {
        header.push_str(&format!(
            "\n采集错误｜{}",
            truncate(&health.last_error, 160)
        ));
    }
    let blocks = models
        .iter()
        .map(|model| format_model(model))
        .collect::<Vec<_>>();
    Ok(chunk_reports(
        &header,
        &blocks,
        config.bot.max_message_chars,
    ))
}

pub fn format_model_list(config: &AppConfig, cache: &ReportCache) -> Vec<String> {
    let header = format!("[模型监控白名单]\n共 {} 个模型", config.models.len());
    let blocks = config.models.iter().map(|model| {
        let display = if model.display_name.is_empty() {
            &model.name
        } else {
            &model.display_name
        };
        let groups = if model.groups.is_empty() {
            "全部已发现分组".to_string()
        } else {
            model.groups.join("、")
        };
        format!("- {display} ({})\n  分组: {groups}", model.name)
    });
    let mut blocks = blocks.collect::<Vec<_>>();
    blocks.push(format!(
        "快照时间: {}",
        format_timestamp(cache.generated_at)
    ));
    chunk_reports(&header, &blocks, config.bot.max_message_chars)
}

pub fn format_errors(
    config: &AppConfig,
    cache: &ReportCache,
    window: i64,
    model_query: Option<&str>,
) -> Result<Vec<String>, String> {
    let snapshot = cache
        .windows
        .get(&window)
        .ok_or_else(|| "统计快照尚未生成，请稍后重试".to_string())?;
    let models = select_models(snapshot, model_query)?;
    let header = format!("[模型异常摘要] {}", window_label(window));
    let blocks = models.into_iter().map(|model| {
        let mut text = model.display_name.clone();
        if model.overall.error_codes.is_empty() {
            text.push_str("\n  暂无终态错误分类");
        } else {
            for (code, count) in model.overall.error_codes.iter().take(5) {
                text.push_str(&format!("\n  {code}: {count}"));
            }
        }
        for (chain, count) in model.overall.retry_channel_chains.iter().take(3) {
            text.push_str(&format!("\n  重试链 {chain}: {count} 次"));
        }
        text.push_str(&format!(
            "\n  请求错误率 {:.2}% | 尝试错误率 {:.2}%",
            model.overall.error_rate, model.overall.attempt_error_rate
        ));
        text
    });
    Ok(chunk_reports(
        &header,
        &blocks.collect::<Vec<_>>(),
        config.bot.max_message_chars,
    ))
}

pub fn format_health(
    cache: &ReportCache,
    health: &CollectorHealth,
    database_path: &str,
    push_enabled: bool,
    last_heartbeat_at: i64,
) -> String {
    let push_status = if !push_enabled {
        "未启用".to_string()
    } else if last_heartbeat_at == 0 {
        "等待 Heartbeat".to_string()
    } else {
        format!("最后心跳 {}", format_timestamp(last_heartbeat_at))
    };
    format!(
        "[监控健康]\n数据库: {database_path}\n启动时间: {}\n最后尝试: {}\n最后成功: {}\n连续失败: {}\n日志样本: {}\n请求结果: {}\n推送: {push_status}\n状态: {}",
        format_timestamp(health.started_at),
        format_timestamp(health.last_attempt_at),
        format_timestamp(health.last_success_at),
        health.consecutive_failures,
        cache.database.sample_count,
        cache.database.outcome_count,
        if health.last_error.is_empty() {
            "正常"
        } else {
            &health.last_error
        }
    )
}

pub fn format_perf_metrics(data: &PerfMetricData, hours: u32) -> String {
    let mut text = format!("[模型广场参考] {} | 近{}小时", data.model_name, hours);
    if data.groups.is_empty() {
        text.push_str("\n暂无公开分组样本");
        return text;
    }
    for group in &data.groups {
        let ttft = if group.avg_ttft_ms > 0 {
            format_duration(Some(group.avg_ttft_ms))
        } else {
            "--".to_string()
        };
        text.push_str(&format!(
            "\n{}\n  成功 {:.2}% | 首字 {} | 总耗时 {} | {:.1} tok/s",
            group.group,
            group.success_rate,
            ttft,
            format_duration(Some(group.avg_latency_ms)),
            group.avg_tps,
        ));
    }
    text.push_str("\n注: 广场指标不与本地日志统计混算");
    text
}

fn format_model(model: &ModelReport) -> String {
    let mut text = format!(
        "{} {}｜{}\n{}｜请求{}｜成功{}｜失败{}｜部分{}\n成功率{:.2}%｜错误率{:.2}%｜重试{:.2}%",
        status_icon(model.status),
        model.display_name,
        model.status.label(),
        model.model_name,
        model.overall.requests,
        model.overall.successes,
        model.overall.failures,
        model.overall.partial_failures,
        model.overall.success_rate,
        model.overall.error_rate,
        model.overall.retry_rate,
    );
    if model.groups.is_empty() {
        text.push_str("\n◻️ 暂无分组样本");
    }
    let group_count = model.groups.len();
    for (index, group) in model.groups.iter().enumerate() {
        let metrics = &group.metrics;
        let branch = if index + 1 == group_count {
            "└─"
        } else {
            "├─"
        };
        if metrics.requests == 0 {
            text.push_str(&format!(
                "\n{branch} {} {}｜暂无样本｜请求{}",
                status_icon(group.status),
                group.group_name,
                metrics.requests,
            ));
            continue;
        }
        let detail_prefix = if index + 1 == group_count {
            "   "
        } else {
            "│  "
        };
        text.push_str(&format!(
            "\n{branch} {} {}｜请求{}｜成功{:.2}%｜错误{:.2}%\n{detail_prefix}首字：均{}｜中位{}｜P95 {}\n{detail_prefix}总耗：均{}｜中位{}｜P95 {}",
            status_icon(group.status), group.group_name, metrics.requests, metrics.success_rate,
            metrics.error_rate, format_duration(metrics.avg_ttft_ms), format_duration(metrics.p50_ttft_ms),
            format_duration(metrics.p95_ttft_ms), format_total_duration(metrics.avg_total_ms),
            format_total_duration(metrics.p50_total_ms), format_total_duration(metrics.p95_total_ms),
        ));
    }
    text
}

fn status_icon(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Normal => "✅",
        HealthStatus::Degraded => "⚠️",
        HealthStatus::Abnormal => "❌",
        HealthStatus::Stale => "⏳",
        HealthStatus::NoData | HealthStatus::InsufficientSamples => "◻️",
    }
}

fn format_duration(value: Option<i64>) -> String {
    match value {
        None => "--".to_string(),
        Some(value) if value < 1000 => format!("{value}ms"),
        Some(value) => format!("{:.2}s", value as f64 / 1000.0),
    }
}

fn format_total_duration(value: Option<i64>) -> String {
    match value {
        None => "--".to_string(),
        Some(value) => format!("{}s", value / 1000),
    }
}

fn select_models<'a>(
    snapshot: &'a WindowSnapshot,
    query: Option<&str>,
) -> Result<Vec<&'a ModelReport>, String> {
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return Ok(snapshot.models.iter().collect());
    };
    snapshot
        .models
        .iter()
        .find(|model| {
            model.model_name.eq_ignore_ascii_case(query)
                || model.display_name.eq_ignore_ascii_case(query)
        })
        .map(|model| vec![model])
        .ok_or_else(|| format!("模型不在白名单中: {query}"))
}

#[derive(Default)]
struct StatusCounts {
    normal: usize,
    degraded: usize,
    abnormal: usize,
    stale: usize,
    no_data: usize,
}

fn status_counts(models: &[&ModelReport]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for model in models {
        match model.status {
            HealthStatus::Normal => counts.normal += 1,
            HealthStatus::Degraded => counts.degraded += 1,
            HealthStatus::Abnormal => counts.abnormal += 1,
            HealthStatus::Stale => counts.stale += 1,
            HealthStatus::NoData | HealthStatus::InsufficientSamples => counts.no_data += 1,
        }
    }
    counts
}

fn chunk_reports(header: &str, blocks: &[String], max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(200);
    let mut chunks = Vec::new();
    let mut current = header.to_string();
    for block in blocks {
        if current.chars().count() + 2 + block.chars().count() > max_chars && current != header {
            chunks.push(current);
            current = format!("📊 模型状态｜续\n\n{block}");
        } else {
            current.push_str("\n\n");
            current.push_str(block);
        }
    }
    chunks.push(current);
    chunks
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn format_timestamp(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "--".to_string();
    }
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| timestamp.to_string())
}

pub fn window_label(window: i64) -> &'static str {
    match window {
        60 => "近1分钟",
        300 => "近5分钟",
        600 => "近10分钟",
        900 => "近15分钟",
        3600 => "近1小时",
        86_400 => "近24小时",
        604_800 => "近7天",
        _ => "自定义窗口",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::metrics::{GroupReport, MetricValues, ModelReport, WindowSnapshot};

    #[test]
    fn chunks_on_model_boundary() {
        let blocks = vec!["a".repeat(150), "b".repeat(150)];
        assert_eq!(chunk_reports("header", &blocks, 200).len(), 2);
    }

    #[test]
    fn labels_all_supported_short_windows() {
        assert_eq!(window_label(60), "近1分钟");
        assert_eq!(window_label(300), "近5分钟");
        assert_eq!(window_label(600), "近10分钟");
    }

    #[test]
    fn total_duration_preserves_source_second_precision() {
        assert_eq!(format_total_duration(Some(3000)), "3s");
    }

    #[test]
    fn status_report_uses_compact_model_and_group_layout() {
        let model = ModelReport {
            model_name: "gpt-5.6-sol".to_string(),
            display_name: "GPT-5.6 Sol".to_string(),
            status: HealthStatus::Abnormal,
            overall: MetricValues {
                requests: 38,
                successes: 30,
                failures: 8,
                success_rate: 78.95,
                error_rate: 21.05,
                retry_rate: 57.89,
                ..MetricValues::default()
            },
            groups: vec![
                GroupReport {
                    group_name: "Codex Burst".to_string(),
                    status: HealthStatus::Abnormal,
                    metrics: MetricValues {
                        requests: 24,
                        success_rate: 66.67,
                        error_rate: 33.33,
                        avg_ttft_ms: Some(7960),
                        p50_ttft_ms: Some(5250),
                        p95_ttft_ms: Some(30_100),
                        avg_total_ms: Some(22_000),
                        p50_total_ms: Some(17_000),
                        p95_total_ms: Some(30_000),
                        ..MetricValues::default()
                    },
                },
                GroupReport {
                    group_name: "Codex Plus".to_string(),
                    status: HealthStatus::NoData,
                    metrics: MetricValues::default(),
                },
            ],
        };

        let report = format_model(&model);
        assert!(
            report.starts_with("❌ GPT-5.6 Sol｜异常\ngpt-5.6-sol｜请求38｜成功30｜失败8｜部分0")
        );
        assert!(report.contains("成功率78.95%｜错误率21.05%｜重试57.89%"));
        assert!(report.contains("├─ ❌ Codex Burst｜请求24｜成功66.67%｜错误33.33%"));
        assert!(report.contains("│  首字：均7.96s｜中位5.25s｜P95 30.10s"));
        assert!(report.contains("│  总耗：均22s｜中位17s｜P95 30s"));
        assert!(report.contains("Codex Plus｜暂无样本｜请求0"));
        assert!(!report.contains("尝试错误"));
        assert!(!report.contains("最新"));
        assert!(!report.contains("P50"));
    }

    #[test]
    fn status_header_keeps_compact_counts_and_collector_symbol() {
        let config = AppConfig::default();
        let snapshot = WindowSnapshot {
            generated_at: 1_700_000_000,
            window_seconds: 900,
            models: vec![ModelReport {
                model_name: "echo".to_string(),
                display_name: "Echo".to_string(),
                status: HealthStatus::Normal,
                overall: MetricValues::default(),
                groups: Vec::new(),
            }],
        };
        let cache = ReportCache {
            windows: HashMap::from([(900, snapshot)]),
            ..ReportCache::default()
        };
        let health = CollectorHealth {
            last_success_at: 1_700_000_000,
            ..CollectorHealth::default()
        };
        let report = format_status(&config, &cache, &health, 900, None)
            .unwrap()
            .join("\n");
        assert!(report.contains("📊 模型状态"));
        assert!(report.contains("白名单 1｜✅1 ⚠️0 ❌0 ⏳0 ◻️0"));
        assert!(report.contains("采集 ✅正常"));
        assert!(report.contains("✅ Echo｜正常"));
        assert!(report.contains("◻️ 暂无分组样本"));
        assert_eq!(status_icon(HealthStatus::Normal), "✅");
        assert_eq!(status_icon(HealthStatus::Degraded), "⚠️");
        assert_eq!(status_icon(HealthStatus::Abnormal), "❌");
        assert_eq!(status_icon(HealthStatus::Stale), "⏳");
        assert_eq!(status_icon(HealthStatus::NoData), "◻️");
    }

    #[test]
    fn error_report_includes_aggregated_retry_chain() {
        let config =
            AppConfig::parse(r#"{"api":{"admin_user_id":3},"models":[{"name":"echo"}]}"#).unwrap();
        let cache = ReportCache {
            windows: HashMap::from([(
                900,
                WindowSnapshot {
                    window_seconds: 900,
                    models: vec![ModelReport {
                        model_name: "echo".to_string(),
                        display_name: "echo".to_string(),
                        status: HealthStatus::Degraded,
                        overall: MetricValues {
                            retry_channel_chains: vec![("12->34".to_string(), 2)],
                            ..MetricValues::default()
                        },
                        groups: Vec::new(),
                    }],
                    ..WindowSnapshot::default()
                },
            )]),
            ..ReportCache::default()
        };
        let chunks = format_errors(&config, &cache, 900, None).unwrap();
        assert!(chunks.join("\n").contains("重试链 12->34: 2 次"));
    }
}
