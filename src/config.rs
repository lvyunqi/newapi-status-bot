use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// 插件完整配置。宿主会将 TOML 转换为 JSON 后传入初始化钩子。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub api: ApiConfig,
    pub storage: StorageConfig,
    pub status: StatusConfig,
    pub perf_metrics: PerfMetricsConfig,
    pub bot: BotConfig,
    pub push: PushConfig,
    pub models: Vec<ModelConfig>,
}

impl AppConfig {
    /// 解析并校验宿主传入的插件配置。
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            return Err("缺少 config/plugins/newapi-status-bot.toml".to_string());
        }
        let config: Self =
            serde_json::from_str(raw).map_err(|error| format!("插件配置解析失败: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let base_url = self.api.base_url.trim();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err("api.base_url 必须是 http 或 https URL".to_string());
        }
        if self.api.admin_user_id <= 0 {
            return Err("api.admin_user_id 必须大于 0".to_string());
        }
        if self.api.access_token_env.trim().is_empty() {
            return Err("api.access_token_env 不能为空".to_string());
        }
        if !(10..=3600).contains(&self.api.poll_interval_secs) {
            return Err("api.poll_interval_secs 必须在 10 到 3600 秒之间".to_string());
        }
        if !(1..=100).contains(&self.api.page_size) {
            return Err("api.page_size 必须在 1 到 100 之间".to_string());
        }
        if self.api.max_pages_per_model == 0 {
            return Err("api.max_pages_per_model 必须大于 0".to_string());
        }
        if self.api.initial_backfill_hours == 0 || self.api.initial_backfill_hours > 24 * 30 {
            return Err("api.initial_backfill_hours 必须在 1 到 720 小时之间".to_string());
        }
        if self.api.overlap_secs < 0 || self.api.overlap_secs > 3600 {
            return Err("api.overlap_secs 必须在 0 到 3600 秒之间".to_string());
        }
        if !(1..=120).contains(&self.api.request_timeout_secs) {
            return Err("api.request_timeout_secs 必须在 1 到 120 秒之间".to_string());
        }
        if !(1..=3).contains(&self.perf_metrics.request_timeout_secs) {
            return Err("perf_metrics.request_timeout_secs 必须在 1 到 3 秒之间".to_string());
        }
        if self.perf_metrics.cache_ttl_secs < 0 {
            return Err("perf_metrics.cache_ttl_secs 不能为负数".to_string());
        }
        if !(1..=30).contains(&self.storage.retention_days) {
            return Err("storage.retention_days 必须在 1 到 30 天之间".to_string());
        }
        validate_relative_path(&self.storage.database_file)?;
        parse_window(&self.status.default_window)?;
        if !(0.0..=100.0).contains(&self.status.normal_success_rate)
            || !(0.0..=100.0).contains(&self.status.degraded_success_rate)
            || self.status.normal_success_rate < self.status.degraded_success_rate
        {
            return Err("状态成功率阈值无效".to_string());
        }
        if self.bot.max_message_chars < 200 {
            return Err("bot.max_message_chars 不能小于 200".to_string());
        }
        if self.push.enabled && self.push.interval_secs < 30 {
            return Err("push.interval_secs 不能小于 30 秒".to_string());
        }
        if !matches!(self.push.mode.as_str(), "periodic" | "change" | "anomaly") {
            return Err("push.mode 只能是 periodic、change 或 anomaly".to_string());
        }
        for target in &self.push.targets {
            target.validate()?;
        }
        if self.models.is_empty() {
            return Err("models 至少需要配置一个白名单模型".to_string());
        }

        let mut names = HashSet::new();
        for model in &self.models {
            if model.name.trim().is_empty() {
                return Err("白名单模型名不能为空".to_string());
            }
            if !names.insert(model.name.clone()) {
                return Err(format!("白名单模型重复: {}", model.name));
            }
        }
        Ok(())
    }

    pub fn database_path(&self, data_dir: &Path) -> PathBuf {
        data_dir.join(&self.storage.database_file)
    }
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("storage.database_file 必须是非空相对路径".to_string());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("storage.database_file 不允许包含上级目录".to_string());
    }
    Ok(())
}

/// 将命令窗口转换为秒数。
pub fn parse_window(value: &str) -> Result<i64, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1m" => Ok(60),
        "5m" => Ok(5 * 60),
        "10m" => Ok(10 * 60),
        "15m" => Ok(15 * 60),
        "1h" => Ok(60 * 60),
        "24h" => Ok(24 * 60 * 60),
        "7d" => Ok(7 * 24 * 60 * 60),
        _ => Err(format!("不支持的时间窗口: {value}")),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    pub base_url: String,
    pub admin_user_id: i64,
    pub access_token_env: String,
    pub poll_interval_secs: u64,
    pub initial_backfill_hours: u64,
    pub overlap_secs: i64,
    pub page_size: u32,
    pub max_pages_per_model: u32,
    pub request_timeout_secs: u64,
    pub settlement_grace_secs: i64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://apis.extralink.net".to_string(),
            admin_user_id: 0,
            access_token_env: "NEWAPI_STATUS_ACCESS_TOKEN".to_string(),
            poll_interval_secs: 30,
            initial_backfill_hours: 24,
            overlap_secs: 120,
            page_size: 100,
            max_pages_per_model: 50,
            request_timeout_secs: 15,
            settlement_grace_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub database_file: String,
    pub retention_days: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database_file: "newapi-status-bot/status.db".to_string(),
            retention_days: 7,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StatusConfig {
    pub default_window: String,
    pub minimum_samples: u64,
    pub normal_success_rate: f64,
    pub degraded_success_rate: f64,
    pub default_max_ttft_ms: i64,
    pub default_max_total_ms: i64,
    pub stale_after_secs: i64,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            default_window: "15m".to_string(),
            minimum_samples: 5,
            normal_success_rate: 99.0,
            degraded_success_rate: 95.0,
            default_max_ttft_ms: 12_000,
            default_max_total_ms: 30_000,
            stale_after_secs: 180,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PerfMetricsConfig {
    pub enabled: bool,
    pub cache_ttl_secs: i64,
    pub request_timeout_secs: u64,
    pub default_hours: u32,
}

impl Default for PerfMetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_ttl_secs: 300,
            request_timeout_secs: 3,
            default_hours: 24,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BotConfig {
    pub allowed_group_ids: Vec<String>,
    pub admin_user_ids: Vec<String>,
    pub max_message_chars: usize,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            allowed_group_ids: Vec::new(),
            admin_user_ids: Vec::new(),
            max_message_chars: 1800,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PushConfig {
    pub enabled: bool,
    pub mode: String,
    pub interval_secs: i64,
    pub targets: Vec<PushTarget>,
    pub confirmations: u32,
    pub cooldown_secs: i64,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "change".to_string(),
            interval_secs: 3600,
            targets: Vec::new(),
            confirmations: 2,
            cooldown_secs: 900,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PushTarget {
    pub bot_id: String,
    pub kind: String,
    pub target_id: String,
    pub guild_id: Option<String>,
}

impl PushTarget {
    /// 校验主动推送目标，避免后台线程向宿主提交不可路由的请求。
    fn validate(&self) -> Result<(), String> {
        if self.bot_id.trim().is_empty() {
            return Err("push.targets[].bot_id 不能为空".to_string());
        }
        if self.target_id.trim().is_empty() {
            return Err("push.targets[].target_id 不能为空".to_string());
        }
        if !matches!(
            self.kind.as_str(),
            "private" | "group" | "channel" | "channel_private"
        ) {
            return Err(
                "push.targets[].kind 只能是 private、group、channel 或 channel_private".to_string(),
            );
        }
        Ok(())
    }

    pub fn guild_id(&self) -> Option<&str> {
        self.guild_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub name: String,
    pub display_name: String,
    pub groups: Vec<String>,
    pub max_ttft_ms: Option<i64>,
    pub max_total_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let raw = r#"{"api":{"admin_user_id":3},"models":[{"name":"echo"}]}"#;
        let config = AppConfig::parse(raw).expect("config should parse");
        assert_eq!(config.api.page_size, 100);
        assert_eq!(config.models[0].name, "echo");
    }

    #[test]
    fn rejects_parent_database_path() {
        let raw = r#"{
            "api":{"admin_user_id":3},
            "storage":{"database_file":"../outside.db"},
            "models":[{"name":"echo"}]
        }"#;
        assert!(AppConfig::parse(raw).is_err());
    }

    #[test]
    fn parses_supported_windows() {
        assert_eq!(parse_window("1m").unwrap(), 60);
        assert_eq!(parse_window("5m").unwrap(), 300);
        assert_eq!(parse_window("10m").unwrap(), 600);
        assert_eq!(parse_window("15m").unwrap(), 900);
        assert_eq!(parse_window("7D").unwrap(), 604_800);
        assert!(parse_window("2h").is_err());
    }

    #[test]
    fn rejects_perf_metrics_timeout_over_three_seconds() {
        let raw = r#"{
            "api":{"admin_user_id":3},
            "perf_metrics":{"request_timeout_secs":4},
            "models":[{"name":"echo"}]
        }"#;
        assert!(AppConfig::parse(raw).is_err());
    }

    #[test]
    fn accepts_enabled_push_without_new_targets_for_migration() {
        let raw = r#"{
            "api":{"admin_user_id":3},
            "push":{"enabled":true,"target_group_ids":["10001"],"sender_self_id":"bot"},
            "models":[{"name":"echo"}]
        }"#;
        let config = AppConfig::parse(raw).expect("legacy push fields should be ignored");
        assert!(config.push.enabled);
        assert!(config.push.targets.is_empty());
    }

    #[test]
    fn validates_proactive_push_targets() {
        let raw = r#"{
            "api":{"admin_user_id":3},
            "push":{
                "enabled":true,
                "targets":[{"bot_id":"qq-reverse","kind":"group","target_id":"10001"}]
            },
            "models":[{"name":"echo"}]
        }"#;
        let config = AppConfig::parse(raw).expect("target should be valid");
        assert_eq!(config.push.targets[0].bot_id, "qq-reverse");

        let invalid = raw.replace("\"group\"", "\"guild\"");
        assert!(AppConfig::parse(&invalid).is_err());
    }
}
