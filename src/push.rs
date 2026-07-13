use std::sync::Arc;

use abi_stable_host_api::{BotApi, NoticeRequest, SendBuilder, SendEnqueueStatus};

use crate::config::{PushConfig, PushTarget};
use crate::metrics::{HealthStatus, WindowSnapshot};
use crate::state::{AppState, PushState};

/// 记录宿主 Heartbeat 事件；Heartbeat 仅用于健康观测，不再驱动后台推送。
pub fn record_heartbeat(state: &Arc<AppState>, _request: &NoticeRequest) {
    let now = crate::state::unix_now();
    if let Ok(mut push) = state.push.lock() {
        push.last_heartbeat_at = now;
    }
}

/// 初始化时提示旧配置迁移结果，避免启用推送但实际没有任何实时目标。
pub fn log_missing_targets_on_init(state: &Arc<AppState>) {
    if state.config.push.enabled && state.config.push.targets.is_empty() {
        log_missing_targets_once(state);
    }
}

/// 在采集线程确认快照可用后，按配置把状态报告实时入队到宿主。
pub fn maybe_push(state: &Arc<AppState>, now: i64) {
    let config = &state.config.push;
    if !config.enabled {
        return;
    }
    if config.targets.is_empty() {
        log_missing_targets_once(state);
        return;
    }

    let Ok(default_window) = crate::config::parse_window(&state.config.status.default_window)
    else {
        return;
    };
    let snapshot = match crate::query::load_window_snapshot(
        &state.config,
        &state.database_path,
        default_window,
        now,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("[newapi-status-bot] load push snapshot failed: {error}");
            return;
        }
    };
    let health = state
        .health
        .read()
        .ok()
        .map(|value| value.clone())
        .unwrap_or_default();
    let (fingerprint, has_alert) = snapshot_signal(&snapshot);
    let should_send = {
        let Ok(mut push) = state.push.lock() else {
            return;
        };
        should_send_push(&mut push, config, &fingerprint, has_alert, now)
    };
    if !should_send {
        return;
    }

    let Ok(chunks) = crate::report::format_status(&state.config, &snapshot, &health, None) else {
        return;
    };
    let attempts = enqueue_chunks_with(&config.targets, &chunks, send_text_to_target);
    for attempt in &attempts {
        eprintln!(
            "[newapi-status-bot] proactive push enqueue bot_id={} kind={} target_id={} status={:?}",
            attempt.bot_id, attempt.kind, attempt.target_id, attempt.status
        );
    }

    let accepted = {
        let Ok(mut push) = state.push.lock() else {
            return;
        };
        commit_if_any_accepted(&mut push, fingerprint, has_alert, now, &attempts)
    };
    if !accepted {
        eprintln!("[newapi-status-bot] proactive push not accepted by any configured target");
    }
}

fn log_missing_targets_once(state: &Arc<AppState>) {
    if let Ok(mut push) = state.push.lock()
        && !push.missing_targets_logged
    {
        push.missing_targets_logged = true;
        eprintln!(
            "[newapi-status-bot] 推送未配置: 请使用 [[push.targets]] 显式设置 bot_id/kind/target_id"
        );
    }
}

fn snapshot_signal(snapshot: &WindowSnapshot) -> (String, bool) {
    let fingerprint = snapshot
        .models
        .iter()
        .map(|model| format!("{}:{:?}", model.model_name, model.status))
        .collect::<Vec<_>>()
        .join("|");
    let has_alert = snapshot.models.iter().any(|model| {
        matches!(
            model.status,
            HealthStatus::Stale | HealthStatus::Degraded | HealthStatus::Abnormal
        )
    });
    (fingerprint, has_alert)
}

/// 更新推送确认状态；网络发送只在宿主确认接受入队后才提交状态。
fn should_send_push(
    push: &mut PushState,
    config: &PushConfig,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PushAttempt {
    bot_id: String,
    kind: String,
    target_id: String,
    status: SendEnqueueStatus,
}

fn enqueue_chunks_with(
    targets: &[PushTarget],
    chunks: &[String],
    mut send: impl FnMut(&PushTarget, &str) -> SendEnqueueStatus,
) -> Vec<PushAttempt> {
    let mut attempts = Vec::new();
    for target in targets {
        for chunk in chunks {
            let status = send(target, chunk);
            attempts.push(PushAttempt {
                bot_id: target.bot_id.clone(),
                kind: target.kind.clone(),
                target_id: target.target_id.clone(),
                status,
            });
        }
    }
    attempts
}

fn commit_if_any_accepted(
    push: &mut PushState,
    fingerprint: String,
    has_alert: bool,
    now: i64,
    attempts: &[PushAttempt],
) -> bool {
    if !attempts.iter().any(|attempt| attempt.status.is_accepted()) {
        return false;
    }
    push.last_push_at = now;
    push.last_sent_fingerprint = fingerprint;
    push.last_sent_had_alert = has_alert;
    push.candidate_count = 0;
    push.candidate_fingerprint.clear();
    true
}

fn send_text_to_target(target: &PushTarget, message: &str) -> SendEnqueueStatus {
    match target.kind.as_str() {
        "private" => BotApi::for_bot(&target.bot_id).send_private_msg(&target.target_id, message),
        "group" => BotApi::for_bot(&target.bot_id).send_group_msg(&target.target_id, message),
        "channel" => {
            let builder = SendBuilder::channel(&target.target_id)
                .bot(&target.bot_id)
                .text(message);
            send_builder_with_optional_guild(builder, target)
        }
        "channel_private" => {
            let builder = SendBuilder::channel_private(&target.target_id)
                .bot(&target.bot_id)
                .text(message);
            send_builder_with_optional_guild(builder, target)
        }
        _ => SendEnqueueStatus::InvalidRequest,
    }
}

fn send_builder_with_optional_guild(
    builder: SendBuilder,
    target: &PushTarget,
) -> SendEnqueueStatus {
    match target.guild_id() {
        Some(guild_id) => builder.guild_id(guild_id).try_send(),
        None => builder.try_send(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: &str, id: &str) -> PushTarget {
        PushTarget {
            bot_id: "qq-reverse".to_string(),
            kind: kind.to_string(),
            target_id: id.to_string(),
            guild_id: None,
        }
    }

    #[test]
    fn anomaly_mode_sends_initial_alert_after_confirmation() {
        let mut push = PushState::default();
        let config = PushConfig {
            mode: "anomaly".to_string(),
            confirmations: 2,
            cooldown_secs: 0,
            ..PushConfig::default()
        };

        assert!(!should_send_push(&mut push, &config, "echo:bad", true, 100));
        assert!(should_send_push(&mut push, &config, "echo:bad", true, 101));
    }

    #[test]
    fn enqueue_uses_every_target_and_chunk() {
        let targets = vec![target("group", "10001"), target("private", "20002")];
        let chunks = vec!["first".to_string(), "second".to_string()];

        let attempts = enqueue_chunks_with(&targets, &chunks, |target, message| {
            assert!(!target.bot_id.is_empty());
            assert!(!message.is_empty());
            SendEnqueueStatus::Accepted
        });

        assert_eq!(attempts.len(), 4);
        assert!(attempts.iter().all(|attempt| attempt.status.is_accepted()));
    }

    #[test]
    fn accepted_attempt_commits_push_state() {
        let attempts = vec![PushAttempt {
            bot_id: "qq-reverse".to_string(),
            kind: "group".to_string(),
            target_id: "10001".to_string(),
            status: SendEnqueueStatus::Accepted,
        }];
        let mut push = PushState {
            candidate_count: 2,
            candidate_fingerprint: "old".to_string(),
            ..PushState::default()
        };

        assert!(commit_if_any_accepted(
            &mut push,
            "echo:bad".to_string(),
            true,
            123,
            &attempts
        ));
        assert_eq!(push.last_push_at, 123);
        assert_eq!(push.last_sent_fingerprint, "echo:bad");
        assert!(push.last_sent_had_alert);
        assert_eq!(push.candidate_count, 0);
        assert!(push.candidate_fingerprint.is_empty());
    }

    #[test]
    fn rejected_attempts_do_not_commit_push_state() {
        let attempts = vec![PushAttempt {
            bot_id: "qq-reverse".to_string(),
            kind: "group".to_string(),
            target_id: "10001".to_string(),
            status: SendEnqueueStatus::HostUnavailable,
        }];
        let mut push = PushState::default();

        assert!(!commit_if_any_accepted(
            &mut push,
            "echo:bad".to_string(),
            true,
            123,
            &attempts
        ));
        assert_eq!(push.last_push_at, 0);
        assert!(push.last_sent_fingerprint.is_empty());
    }

    #[test]
    fn channel_target_attaches_optional_guild_context() {
        abi_stable_host_api::unbind_host_api_v1();
        let target = PushTarget {
            guild_id: Some("guild-1".to_string()),
            ..target("channel", "channel-1")
        };
        let status = send_text_to_target(&target, "hello");

        assert_eq!(status, SendEnqueueStatus::HostUnavailable);
    }
}
