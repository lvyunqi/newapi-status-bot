use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::api::RemoteLog;

pub const LOG_TYPE_CONSUME: i64 = 2;
pub const LOG_TYPE_ERROR: i64 = 5;
pub const UNKNOWN_GROUP: &str = "自动路由/未确认";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Consume,
    Error,
}

impl SampleKind {
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Consume => LOG_TYPE_CONSUME,
            Self::Error => LOG_TYPE_ERROR,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogSample {
    pub sample_key: String,
    pub source_log_id: i64,
    pub request_id: String,
    pub upstream_request_id: String,
    pub created_at: i64,
    pub kind: SampleKind,
    pub model_name: String,
    pub group_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub is_stream: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub partial_failure: bool,
    pub attempt_index: i64,
    pub error_code: String,
    pub status_code: Option<i64>,
    pub error_message: String,
}

impl LogSample {
    /// 将远端日志转换为最小化、脱敏、可去重的本地样本。
    pub fn from_remote(remote: RemoteLog) -> Option<Self> {
        let kind = match remote.log_type {
            LOG_TYPE_CONSUME => SampleKind::Consume,
            LOG_TYPE_ERROR => SampleKind::Error,
            _ => return None,
        };
        if remote.model_name.trim().is_empty() {
            return None;
        }

        let other = parse_other(&remote.other);
        let total_ms = (remote.use_time >= 0).then(|| remote.use_time.saturating_mul(1000));
        let raw_ttft = other
            .get("frt")
            .and_then(Value::as_f64)
            .map(|value| value.round() as i64);
        let ttft_ms = raw_ttft.filter(|value| {
            *value >= 0 && total_ms.is_none_or(|total| *value <= total.saturating_add(2000))
        });
        let stream_status = other
            .pointer("/stream_status/status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let partial_failure = kind == SampleKind::Consume
            && (stream_status.eq_ignore_ascii_case("error")
                || other
                    .pointer("/stream_status/error_count")
                    .and_then(Value::as_i64)
                    > Some(0));
        let attempt_index = other
            .pointer("/admin_info/use_channel")
            .and_then(Value::as_array)
            .map(|channels| channels.len() as i64)
            .filter(|value| *value > 0)
            .unwrap_or(1);
        let error_code = other
            .get("error_code")
            .and_then(value_as_string)
            .unwrap_or_default();
        let status_code = other.get("status_code").and_then(Value::as_i64);
        let group_name = if remote.group.trim().is_empty() {
            UNKNOWN_GROUP.to_string()
        } else {
            remote.group.clone()
        };
        let request_id = if remote.request_id.trim().is_empty() {
            format!(
                "legacy:{}:{}:{}:{}",
                remote.created_at, remote.model_name, remote.channel_id, remote.id
            )
        } else {
            remote.request_id.clone()
        };
        let sample_key = sample_key(
            &request_id,
            kind,
            remote.channel_id,
            attempt_index,
            remote.created_at,
            &remote.upstream_request_id,
            &remote.content,
        );

        Some(Self {
            sample_key,
            source_log_id: remote.id,
            request_id,
            upstream_request_id: remote.upstream_request_id,
            created_at: remote.created_at,
            kind,
            model_name: remote.model_name,
            group_name,
            channel_id: remote.channel_id,
            channel_name: remote.channel_name,
            is_stream: remote.is_stream,
            prompt_tokens: remote.prompt_tokens,
            completion_tokens: remote.completion_tokens,
            total_ms,
            ttft_ms,
            partial_failure,
            attempt_index,
            error_code,
            status_code,
            error_message: if kind == SampleKind::Error {
                redact_error(&remote.content)
            } else {
                String::new()
            },
        })
    }
}

fn parse_other(value: &Value) -> Value {
    match value {
        Value::String(raw) if !raw.trim().is_empty() => {
            serde_json::from_str(raw).unwrap_or(Value::Null)
        }
        Value::Object(_) => value.clone(),
        _ => Value::Null,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn sample_key(
    request_id: &str,
    kind: SampleKind,
    channel_id: i64,
    attempt_index: i64,
    created_at: i64,
    upstream_request_id: &str,
    content: &str,
) -> String {
    let identity = match kind {
        SampleKind::Consume => format!("{request_id}|2"),
        SampleKind::Error => format!(
            "{request_id}|5|{channel_id}|{attempt_index}|{created_at}|{upstream_request_id}|{content}"
        ),
    };
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

/// 删除常见访问令牌和 URL 查询串，并限制本地错误摘要长度。
pub fn redact_error(value: &str) -> String {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    static QUERY: OnceLock<Regex> = OnceLock::new();
    let secret = SECRET.get_or_init(|| {
        Regex::new(r"(?i)(bearer\s+|sk-)[a-z0-9_\-]{8,}").expect("valid secret regex")
    });
    let query = QUERY.get_or_init(|| {
        Regex::new(r"(?i)(token|key|secret|authorization)=([^&\s]+)").expect("valid query regex")
    });
    let redacted = secret.replace_all(value, "$1[REDACTED]");
    let redacted = query.replace_all(&redacted, "$1=[REDACTED]");
    redacted.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(log_type: i64, other: Value) -> RemoteLog {
        RemoteLog {
            id: 1,
            created_at: 100,
            log_type,
            content: "upstream failed with sk-secretvalue123".to_string(),
            model_name: "echo".to_string(),
            prompt_tokens: 1,
            completion_tokens: 2,
            use_time: 3,
            is_stream: true,
            channel_id: 7,
            channel_name: "primary".to_string(),
            group: "default".to_string(),
            request_id: "req-1".to_string(),
            upstream_request_id: String::new(),
            other,
        }
    }

    #[test]
    fn parses_ttft_and_partial_stream() {
        let sample = LogSample::from_remote(remote(
            LOG_TYPE_CONSUME,
            serde_json::json!("{\"frt\":1200,\"stream_status\":{\"status\":\"error\"}}"),
        ))
        .unwrap();
        assert_eq!(sample.ttft_ms, Some(1200));
        assert!(sample.partial_failure);
    }

    #[test]
    fn rejects_impossible_ttft() {
        let sample =
            LogSample::from_remote(remote(LOG_TYPE_CONSUME, serde_json::json!({"frt": 9000})))
                .unwrap();
        assert_eq!(sample.ttft_ms, None);
    }

    #[test]
    fn redacts_tokens() {
        let result = redact_error("Bearer abcdefghijkl and sk-abcdefghijk");
        assert!(!result.contains("abcdefghijkl"));
        assert!(!result.contains("abcdefghijk"));
    }
}
