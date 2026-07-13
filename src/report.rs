use crate::api::PerfMetricData;
use crate::config::AppConfig;
use crate::metrics::{HealthStatus, ModelReport, WindowSnapshot};
use crate::repository::DatabaseStats;
use crate::state::CollectorHealth;

pub fn format_status(
    config: &AppConfig,
    snapshot: &WindowSnapshot,
    health: &CollectorHealth,
    model_query: Option<&str>,
) -> Result<Vec<String>, String> {
    let models = select_models(snapshot, model_query)?;
    let stale = health.last_success_at == 0
        || snapshot.generated_at.saturating_sub(health.last_success_at)
            > config.status.stale_after_secs;
    let collector_status = if stale {
        "⏳数据过期"
    } else {
        "✅正常"
    };
    let mut header = format!(
        "📊 模型状态｜{}\n🕒 {} - {}｜采集 {}",
        window_label(snapshot.window_seconds),
        format_timestamp(snapshot.generated_at),
        format_time(health.last_success_at),
        collector_status,
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

pub fn format_model_list(config: &AppConfig, generated_at: i64) -> Vec<String> {
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
    blocks.push(format!("快照时间: {}", format_timestamp(generated_at)));
    chunk_reports(&header, &blocks, config.bot.max_message_chars)
}

pub fn format_errors(
    config: &AppConfig,
    snapshot: &WindowSnapshot,
    model_query: Option<&str>,
) -> Result<Vec<String>, String> {
    let models = select_models(snapshot, model_query)?;
    let header = format!("[模型异常摘要] {}", window_label(snapshot.window_seconds));
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
    database: &DatabaseStats,
    health: &CollectorHealth,
    database_path: &str,
    push_enabled: bool,
    targets_configured: bool,
    last_push_at: i64,
    last_heartbeat_at: i64,
) -> String {
    let push_status = if !push_enabled {
        "未启用".to_string()
    } else if !targets_configured {
        "推送未配置".to_string()
    } else if last_push_at == 0 {
        "已配置，等待状态确认".to_string()
    } else {
        format!("最后入队 {}", format_timestamp(last_push_at))
    };
    let heartbeat_status = if last_heartbeat_at == 0 {
        "未收到".to_string()
    } else {
        format!("最后收到 {}", format_timestamp(last_heartbeat_at))
    };
    format!(
        "[监控健康]\n数据库: {database_path}\n启动时间: {}\n最后尝试: {}\n最后成功: {}\n连续失败: {}\n日志样本: {}\n请求结果: {}\n推送: {push_status}\n心跳: {heartbeat_status}\n状态: {}",
        format_timestamp(health.started_at),
        format_timestamp(health.last_attempt_at),
        format_timestamp(health.last_success_at),
        health.consecutive_failures,
        database.sample_count,
        database.outcome_count,
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
        "{} {}｜{}",
        status_icon(model.status),
        model.display_name,
        status_label(model.status),
    );

    if model.overall.requests > 0 {
        text.push_str(&format!("\n成功率{:.2}%", model.overall.success_rate));
    }

    for group in &model.groups {
        let metrics = &group.metrics;
        if metrics.requests == 0 {
            text.push_str(&format!("\n✅ {}｜正常", group.group_name));
            continue;
        }

        text.push_str(&format!(
            "\n{} {}｜成功{:.2}%\n首字：均{}｜中位{}｜P95 {}\n总耗：均{}｜中位{}｜P95 {}",
            status_icon(group.status),
            group.group_name,
            metrics.success_rate,
            format_duration(metrics.avg_ttft_ms),
            format_duration(metrics.p50_ttft_ms),
            format_duration(metrics.p95_ttft_ms),
            format_total_duration(metrics.avg_total_ms),
            format_total_duration(metrics.p50_total_ms),
            format_total_duration(metrics.p95_total_ms),
        ));
    }
    text
}

fn status_icon(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Normal | HealthStatus::NoData => "✅",
        HealthStatus::InsufficientSamples => "◻️",
        HealthStatus::Degraded => "⚠️",
        HealthStatus::Abnormal => "❌",
        HealthStatus::Stale => "⏳",
    }
}

fn status_label(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::NoData => "正常",
        _ => status.label(),
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
    let timezone =
        chrono::FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 display offset must be valid");
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            value
                .with_timezone(&timezone)
                .format("%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| timestamp.to_string())
}

fn format_time(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "--".to_string();
    }
    let timezone =
        chrono::FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 display offset must be valid");
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            value
                .with_timezone(&timezone)
                .format("%H:%M:%S")
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
    fn timestamp_uses_utc_plus_eight() {
        assert_eq!(format_timestamp(1_700_000_000), "11-15 06:13:20");
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
        assert_eq!(
            report,
            "❌ GPT-5.6 Sol｜异常\n成功率78.95%\n❌ Codex Burst｜成功66.67%\n首字：均7.96s｜中位5.25s｜P95 30.10s\n总耗：均22s｜中位17s｜P95 30s\n✅ Codex Plus｜正常"
        );
        for removed in [
            "gpt-5.6-sol",
            "请求",
            "失败",
            "部分",
            "错误",
            "重试",
            "├─",
            "└─",
            "│",
            "P50",
            "暂无样本",
        ] {
            assert!(!report.contains(removed), "unexpected field: {removed}");
        }
    }

    #[test]
    fn status_header_omits_counts_and_separates_models() {
        let config = AppConfig::default();
        let snapshot = WindowSnapshot {
            generated_at: 1_700_000_000,
            window_seconds: 900,
            models: vec![
                ModelReport {
                    model_name: "echo".to_string(),
                    display_name: "Echo".to_string(),
                    status: HealthStatus::Normal,
                    overall: MetricValues::default(),
                    groups: Vec::new(),
                },
                ModelReport {
                    model_name: "empty".to_string(),
                    display_name: "Empty".to_string(),
                    status: HealthStatus::NoData,
                    overall: MetricValues::default(),
                    groups: Vec::new(),
                },
            ],
        };
        let health = CollectorHealth {
            last_success_at: 1_699_999_990,
            ..CollectorHealth::default()
        };
        let report = format_status(&config, &snapshot, &health, None)
            .unwrap()
            .join("\n");
        assert!(report.contains("📊 模型状态"));
        assert!(report.contains("🕒 11-15 06:13:20 - 06:13:10｜采集 ✅正常"));
        assert!(report.contains("✅ Echo｜正常\n\n✅ Empty｜正常"));
        assert!(!report.contains("查询"));
        assert!(!report.contains("数据截至"));
        assert!(!report.contains("白名单"));
        assert!(!report.contains("请求"));
        assert!(!report.contains("暂无分组样本"));
        assert_eq!(status_icon(HealthStatus::Normal), "✅");
        assert_eq!(status_icon(HealthStatus::Degraded), "⚠️");
        assert_eq!(status_icon(HealthStatus::Abnormal), "❌");
        assert_eq!(status_icon(HealthStatus::Stale), "⏳");
        assert_eq!(status_icon(HealthStatus::NoData), "✅");
        assert_eq!(status_icon(HealthStatus::InsufficientSamples), "◻️");
        assert_eq!(status_label(HealthStatus::NoData), "正常");
    }

    #[test]
    fn error_report_includes_aggregated_retry_chain() {
        let config =
            AppConfig::parse(r#"{"api":{"admin_user_id":3},"models":[{"name":"echo"}]}"#).unwrap();
        let snapshot = WindowSnapshot {
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
        };
        let chunks = format_errors(&config, &snapshot, None).unwrap();
        assert!(chunks.join("\n").contains("重试链 12->34: 2 次"));
    }
}
