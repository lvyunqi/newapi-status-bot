mod api;
mod collector;
mod config;
mod domain;
mod metrics;
mod report;
mod repository;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use abi_stable_host_api::{
    BotApi, CommandRequest, CommandResponse, DynamicActionResponse, NoticeRequest, NoticeResponse,
    PluginInitConfig, PluginInitResult,
};
use qimen_dynamic_plugin_derive::dynamic_plugin;

use crate::config::AppConfig;
use crate::state::AppState;

#[dynamic_plugin(id = "newapi-status-bot", version = "0.1.0")]
mod plugin {
    use super::*;

    #[init]
    fn on_init(init: PluginInitConfig) -> PluginInitResult {
        match initialize(init) {
            Ok(()) => PluginInitResult::ok(),
            Err(error) => PluginInitResult::err(&error),
        }
    }

    #[shutdown]
    fn on_shutdown() {
        if let Some(state) = crate::state::take() {
            state.shutdown();
        }
    }

    #[command(
        name = "模型状态",
        description = "查询白名单模型和分组运行状态",
        aliases = "状态,model-status",
        category = "monitoring",
        scope = "all"
    )]
    fn model_status(request: &CommandRequest) -> CommandResponse {
        let Some(state) = allowed_state(request) else {
            return CommandResponse::text("当前会话未启用模型监控查询");
        };
        let (window, model) = match parse_query(&state.config, request.args.as_str()) {
            Ok(query) => query,
            Err(error) => return CommandResponse::text(&error),
        };
        let cache = state
            .reports
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        let health = state
            .health
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        let mut chunks = match crate::report::format_status(
            &state.config,
            &cache,
            &health,
            window,
            model.as_deref(),
        ) {
            Ok(chunks) => chunks,
            Err(error) => return CommandResponse::text(&error),
        };
        if let Some(model) = model
            && state.config.perf_metrics.enabled
        {
            let hours = window_hours(&state.config, window);
            match get_perf_metrics(&state, &model, hours) {
                Ok(data) => chunks.push(crate::report::format_perf_metrics(&data, hours)),
                Err(error) => chunks.push(format!("[模型广场参考]\n获取失败: {error}")),
            }
        }
        respond_chunks(request, chunks)
    }

    #[command(
        name = "模型列表",
        description = "显示本地监控模型白名单",
        aliases = "model-list",
        category = "monitoring",
        scope = "all"
    )]
    fn model_list(request: &CommandRequest) -> CommandResponse {
        let Some(state) = allowed_state(request) else {
            return CommandResponse::text("当前会话未启用模型监控查询");
        };
        let cache = state
            .reports
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        let chunks = crate::report::format_model_list(&state.config, &cache);
        respond_chunks(request, chunks)
    }

    #[command(
        name = "模型异常",
        description = "查看白名单模型脱敏错误分类",
        aliases = "model-errors",
        category = "monitoring",
        scope = "all"
    )]
    fn model_errors(request: &CommandRequest) -> CommandResponse {
        let Some(state) = allowed_state(request) else {
            return CommandResponse::text("当前会话未启用模型监控查询");
        };
        let (window, model) = match parse_query(&state.config, request.args.as_str()) {
            Ok(query) => query,
            Err(error) => return CommandResponse::text(&error),
        };
        let cache = state
            .reports
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        match crate::report::format_errors(&state.config, &cache, window, model.as_deref()) {
            Ok(chunks) => respond_chunks(request, chunks),
            Err(error) => CommandResponse::text(&error),
        }
    }

    #[command(
        name = "监控健康",
        description = "显示模型监控采集器状态",
        aliases = "monitor-health",
        category = "monitoring",
        role = "admin"
    )]
    fn monitor_health(request: &CommandRequest) -> CommandResponse {
        let Some(state) = crate::state::current() else {
            return CommandResponse::text("监控插件尚未初始化");
        };
        if !admin_allowed(&state.config, request.sender_id.as_str()) {
            return CommandResponse::text("无权查看监控运行状态");
        }
        let health = state
            .health
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        let cache = state
            .reports
            .read()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        let last_heartbeat_at = state
            .push
            .lock()
            .map(|push| push.last_heartbeat_at)
            .unwrap_or_default();
        CommandResponse::text(&crate::report::format_health(
            &cache,
            &health,
            &state.database_path.display().to_string(),
            state.config.push.enabled,
            last_heartbeat_at,
        ))
    }

    #[command(
        name = "监控刷新",
        description = "立即唤醒模型监控采集器",
        aliases = "monitor-refresh",
        category = "monitoring",
        role = "admin"
    )]
    fn monitor_refresh(request: &CommandRequest) -> CommandResponse {
        let Some(state) = crate::state::current() else {
            return CommandResponse::text("监控插件尚未初始化");
        };
        if !admin_allowed(&state.config, request.sender_id.as_str()) {
            return CommandResponse::text("无权刷新监控数据");
        }
        state.control.request_refresh();
        CommandResponse::text("已唤醒后台采集器")
    }

    #[route(kind = "meta", events = "Heartbeat")]
    fn heartbeat(request: &NoticeRequest) -> NoticeResponse {
        if let Some(state) = crate::state::current() {
            handle_heartbeat(&state, request);
        }
        NoticeResponse {
            action: DynamicActionResponse::ignore(),
        }
    }
}

fn initialize(init: PluginInitConfig) -> Result<(), String> {
    let config = AppConfig::parse(init.config_json.as_str())?;
    let data_dir = if init.data_dir.is_empty() {
        PathBuf::from("data")
    } else {
        PathBuf::from(init.data_dir.as_str())
    };
    let database_path = config.database_path(&data_dir);
    crate::repository::Repository::open(&database_path)?;
    let state = AppState::new(config, database_path);
    state.start_worker()?;
    if let Err(error) = crate::state::install(state.clone()) {
        state.shutdown();
        return Err(error);
    }
    Ok(())
}

fn allowed_state(request: &CommandRequest) -> Option<Arc<AppState>> {
    let state = crate::state::current()?;
    query_allowed(
        &state.config,
        request.group_id.as_str(),
        request.sender_id.as_str(),
    )
    .then_some(state)
}

/// 群聊使用群白名单，私聊使用管理员用户白名单；空白名单保持不过滤的兼容语义。
fn query_allowed(config: &AppConfig, group_id: &str, sender_id: &str) -> bool {
    if group_id.is_empty() {
        admin_allowed(config, sender_id)
    } else {
        group_allowed(config, group_id)
    }
}

fn group_allowed(config: &AppConfig, group_id: &str) -> bool {
    config.bot.allowed_group_ids.is_empty()
        || config
            .bot
            .allowed_group_ids
            .iter()
            .any(|allowed| allowed == group_id)
}

fn admin_allowed(config: &AppConfig, user_id: &str) -> bool {
    config.bot.admin_user_ids.is_empty()
        || config
            .bot
            .admin_user_ids
            .iter()
            .any(|allowed| allowed == user_id)
}

fn parse_query(config: &AppConfig, args: &str) -> Result<(i64, Option<String>), String> {
    let mut window = crate::config::parse_window(&config.status.default_window)?;
    let mut model_parts = Vec::new();
    for part in args.split_whitespace() {
        if let Ok(parsed) = crate::config::parse_window(part) {
            window = parsed;
        } else {
            model_parts.push(part);
        }
    }
    let model = (!model_parts.is_empty()).then(|| model_parts.join(" "));
    if let Some(query) = &model
        && !config.models.iter().any(|model| {
            model.name.eq_ignore_ascii_case(query) || model.display_name.eq_ignore_ascii_case(query)
        })
    {
        return Err(format!("模型不在白名单中: {query}"));
    }
    Ok((window, model))
}

fn window_hours(config: &AppConfig, window: i64) -> u32 {
    if window < 3600 {
        config.perf_metrics.default_hours.clamp(1, 720)
    } else {
        (window / 3600).clamp(1, 720) as u32
    }
}

fn get_perf_metrics(
    state: &Arc<AppState>,
    model: &str,
    hours: u32,
) -> Result<crate::api::PerfMetricData, crate::api::ApiError> {
    let now = crate::state::unix_now();
    let key = (model.to_string(), hours);
    if let Some(cached) = state
        .perf_cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
        && now - cached.fetched_at <= state.config.perf_metrics.cache_ttl_secs
    {
        return Ok(cached.data);
    }
    let data = crate::api::fetch_perf_metrics(
        &state.config.api.base_url,
        &state.config.perf_metrics,
        model,
        hours,
    )?;
    if let Ok(mut cache) = state.perf_cache.lock() {
        cache.insert(
            key,
            crate::state::PerfCacheEntry {
                fetched_at: now,
                data: data.clone(),
            },
        );
    }
    Ok(data)
}

/// 单段由宿主回复原会话，多段则根据请求来源主动发送到对应私聊或群聊。
fn respond_chunks(request: &CommandRequest, chunks: Vec<String>) -> CommandResponse {
    if chunks.len() <= 1 {
        return CommandResponse::text(chunks.first().map(String::as_str).unwrap_or("暂无数据"));
    }
    for chunk in chunks {
        if request.group_id.is_empty() {
            BotApi::send_private_msg(request.sender_id.as_str(), &chunk);
        } else {
            BotApi::send_group_msg(request.group_id.as_str(), &chunk);
        }
    }
    CommandResponse::ignore()
}

fn handle_heartbeat(state: &Arc<AppState>, request: &NoticeRequest) {
    let config = &state.config.push;
    if !config.enabled {
        return;
    }
    let now = crate::state::unix_now();
    let raw: serde_json::Value =
        serde_json::from_str(request.raw_event_json.as_str()).unwrap_or_default();
    let self_id = raw
        .get("self_id")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|id| id.to_string()))
        })
        .unwrap_or_default();
    if !config.sender_self_id.is_empty() && config.sender_self_id != self_id {
        return;
    }
    let reports = state
        .reports
        .read()
        .ok()
        .map(|value| value.clone())
        .unwrap_or_default();
    let health = state
        .health
        .read()
        .ok()
        .map(|value| value.clone())
        .unwrap_or_default();
    let Ok(default_window) = crate::config::parse_window(&state.config.status.default_window)
    else {
        return;
    };
    let Some(snapshot) = reports.windows.get(&default_window) else {
        return;
    };
    let fingerprint = snapshot
        .models
        .iter()
        .map(|model| format!("{}:{:?}", model.model_name, model.status))
        .collect::<Vec<_>>()
        .join("|");
    let has_alert = snapshot.models.iter().any(|model| {
        matches!(
            model.status,
            crate::metrics::HealthStatus::Stale
                | crate::metrics::HealthStatus::Degraded
                | crate::metrics::HealthStatus::Abnormal
        )
    });

    let should_send = {
        let Ok(mut push) = state.push.lock() else {
            return;
        };
        push.last_heartbeat_at = now;
        should_send_push(&mut push, config, &fingerprint, has_alert, now)
    };
    if !should_send {
        return;
    }
    let Ok(chunks) =
        crate::report::format_status(&state.config, &reports, &health, default_window, None)
    else {
        return;
    };
    for group in &config.target_group_ids {
        for chunk in &chunks {
            BotApi::send_group_msg(group, chunk);
        }
    }
    if let Ok(mut push) = state.push.lock() {
        push.last_push_at = now;
        push.last_sent_fingerprint = fingerprint;
        push.last_sent_had_alert = has_alert;
        push.candidate_count = 0;
    }
}

/// 更新推送确认状态；网络发送只在调用方判定为 true 后发生。
fn should_send_push(
    push: &mut crate::state::PushState,
    config: &crate::config::PushConfig,
    fingerprint: &str,
    has_alert: bool,
    now: i64,
) -> bool {
    if config.mode == "periodic" {
        return now - push.last_push_at >= config.interval_secs;
    }
    if push.last_sent_fingerprint.is_empty() && config.mode == "change" {
        push.last_sent_fingerprint = fingerprint.to_string();
        push.last_sent_had_alert = has_alert;
        return false;
    }
    if push.candidate_fingerprint == fingerprint {
        push.candidate_count = push.candidate_count.saturating_add(1);
    } else {
        push.candidate_fingerprint = fingerprint.to_string();
        push.candidate_count = 1;
    }
    let confirmed = push.candidate_count >= config.confirmations.max(1)
        && now - push.last_push_at >= config.cooldown_secs;
    let changed = fingerprint != push.last_sent_fingerprint;
    confirmed && changed && (config.mode == "change" || has_alert || push.last_sent_had_alert)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_request(sender_id: &str, group_id: &str) -> CommandRequest {
        CommandRequest {
            args: "".into(),
            command_name: "模型状态".into(),
            sender_id: sender_id.into(),
            group_id: group_id.into(),
            raw_event_json: "{}".into(),
            sender_nickname: "tester".into(),
            message_id: "1".into(),
            timestamp: 1,
        }
    }

    fn config() -> AppConfig {
        AppConfig::parse(
            r#"{"api":{"admin_user_id":3},"models":[{"name":"echo","display_name":"Echo Model"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_window_and_model_in_any_order() {
        let (window, model) = parse_query(&config(), "echo 24h").unwrap();
        assert_eq!(window, 86_400);
        assert_eq!(model.as_deref(), Some("echo"));
    }

    #[test]
    fn rejects_model_outside_whitelist() {
        assert!(parse_query(&config(), "private-model").is_err());
    }

    #[test]
    fn exports_expected_dynamic_plugin_descriptor() {
        // 描述符是宿主加载 DLL 时读取的首个 ABI 契约。
        let descriptor = unsafe { qimen_plugin_descriptor() };
        assert_eq!(descriptor.plugin_id.as_str(), "newapi-status-bot");
        assert_eq!(descriptor.api_version.as_str(), "0.3");
        assert_eq!(descriptor.commands.len(), 5);
        for name in ["模型状态", "模型列表", "模型异常"] {
            let command = descriptor
                .commands
                .iter()
                .find(|command| command.name.as_str() == name)
                .expect("query command should be exported");
            assert_eq!(command.scope.as_str(), "all");
        }
        assert!(
            descriptor.routes.iter().any(|route| {
                route.kind.as_str() == "meta" && route.route.as_str() == "Heartbeat"
            })
        );
    }

    #[test]
    fn query_access_uses_group_and_private_allowlists() {
        let mut config = config();
        config.bot.allowed_group_ids = vec!["20001".to_string()];
        config.bot.admin_user_ids = vec!["10001".to_string()];

        assert!(query_allowed(&config, "20001", "someone"));
        assert!(!query_allowed(&config, "20002", "10001"));
        assert!(query_allowed(&config, "", "10001"));
        assert!(!query_allowed(&config, "", "10002"));
    }

    #[test]
    fn multi_chunk_response_targets_private_sender() {
        abi_stable_host_api::drain_send_queue();
        let request = command_request("10001", "");

        respond_chunks(&request, vec!["first".to_string(), "second".to_string()]);

        let actions = abi_stable_host_api::drain_send_queue();
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|action| {
            action.message_type.as_str() == "private" && action.target_id.as_str() == "10001"
        }));
    }

    #[test]
    fn anomaly_mode_sends_initial_alert_after_confirmation() {
        let mut push = crate::state::PushState::default();
        let config = crate::config::PushConfig {
            mode: "anomaly".to_string(),
            confirmations: 2,
            cooldown_secs: 0,
            ..crate::config::PushConfig::default()
        };

        assert!(!should_send_push(&mut push, &config, "echo:bad", true, 100));
        assert!(should_send_push(&mut push, &config, "echo:bad", true, 101));
    }
}
