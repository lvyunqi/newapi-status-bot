use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, RETRY_AFTER};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::config::{ApiConfig, PerfMetricsConfig};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("[configuration] 缺少环境变量 {0}")]
    MissingToken(String),
    #[error("[timeout] HTTP 请求超时")]
    Timeout,
    #[error("[transport] HTTP 请求失败: {0}")]
    Transport(String),
    #[error("[authentication] New API 管理鉴权失败")]
    Authentication,
    #[error("[rate_limit] New API 触发限流")]
    RateLimited(Option<u64>),
    #[error("[server] New API 返回 HTTP {0}")]
    Server(StatusCode),
    #[error("[http] New API 返回 HTTP {0}")]
    Http(StatusCode),
    #[error("[business] New API 业务错误: {0}")]
    Business(String),
    #[error("[protocol] New API 响应缺少 data")]
    MissingData,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct ApiEnvelope<T> {
    success: bool,
    message: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct PageData<T> {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub items: Vec<T>,
}

/// New API 管理日志响应中用于监控的字段。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteLog {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(rename = "type", default)]
    pub log_type: i64,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub model_name: String,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub use_time: i64,
    #[serde(default)]
    pub is_stream: bool,
    #[serde(rename = "channel", default)]
    pub channel_id: i64,
    #[serde(default)]
    pub channel_name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub upstream_request_id: String,
    #[serde(default)]
    pub other: Value,
}

pub struct NewApiClient {
    client: Client,
    base_url: String,
    admin_user_id: i64,
    access_token: String,
}

impl NewApiClient {
    pub fn new(config: &ApiConfig) -> Result<Self, ApiError> {
        let access_token = std::env::var(&config.access_token_env)
            .map_err(|_| ApiError::MissingToken(config.access_token_env.clone()))?;
        if access_token.trim().is_empty() {
            return Err(ApiError::MissingToken(config.access_token_env.clone()));
        }
        Self::with_access_token(config, access_token)
    }

    pub(crate) fn with_access_token(
        config: &ApiConfig,
        access_token: String,
    ) -> Result<Self, ApiError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5.min(config.request_timeout_secs)))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .user_agent("newapi-status-bot/0.1")
            .build()
            .map_err(classify_transport)?;
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            admin_user_id: config.admin_user_id,
            access_token,
        })
    }

    pub fn fetch_logs(
        &self,
        model: &str,
        start_timestamp: i64,
        end_timestamp: i64,
        page: u32,
        page_size: u32,
    ) -> Result<PageData<RemoteLog>, ApiError> {
        let response = self
            .client
            .get(format!("{}/api/log/", self.base_url))
            // New API 管理中间件会直接按原值匹配 users.access_token。
            .header(AUTHORIZATION, &self.access_token)
            .header("New-Api-User", self.admin_user_id.to_string())
            .query(&[
                ("p", page.to_string()),
                ("page_size", page_size.to_string()),
                ("start_timestamp", start_timestamp.to_string()),
                ("end_timestamp", end_timestamp.to_string()),
                ("model_name", model.to_string()),
            ])
            .send()
            .map_err(classify_transport)?;
        if !response.status().is_success() {
            return Err(classify_http(&response));
        }
        unpack(
            response
                .json::<ApiEnvelope<PageData<RemoteLog>>>()
                .map_err(classify_transport)?,
        )
    }
}

fn unpack<T>(envelope: ApiEnvelope<T>) -> Result<T, ApiError> {
    if !envelope.success {
        let message = envelope.message.unwrap_or_else(|| "未知错误".to_string());
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("unauthorized")
            || normalized.contains("access token")
            || message.contains("未登录")
            || message.contains("无权限")
        {
            return Err(ApiError::Authentication);
        }
        return Err(ApiError::Business(message));
    }
    envelope.data.ok_or(ApiError::MissingData)
}

fn classify_transport(error: reqwest::Error) -> ApiError {
    if error.is_timeout() {
        ApiError::Timeout
    } else {
        ApiError::Transport(error.to_string())
    }
}

fn classify_http(response: &reqwest::blocking::Response) -> ApiError {
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ApiError::Authentication,
        StatusCode::TOO_MANY_REQUESTS => {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            ApiError::RateLimited(retry_after)
        }
        status if status.is_server_error() => ApiError::Server(status),
        status => ApiError::Http(status),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerfMetricGroup {
    pub group: String,
    pub avg_ttft_ms: i64,
    pub avg_latency_ms: i64,
    pub success_rate: f64,
    pub avg_tps: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerfMetricData {
    pub model_name: String,
    #[serde(default)]
    pub groups: Vec<PerfMetricGroup>,
}

pub fn fetch_perf_metrics(
    base_url: &str,
    config: &PerfMetricsConfig,
    model: &str,
    hours: u32,
) -> Result<PerfMetricData, ApiError> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(config.request_timeout_secs.min(2)))
        .timeout(Duration::from_secs(config.request_timeout_secs))
        .user_agent("newapi-status-bot/0.1")
        .build()
        .map_err(classify_transport)?;
    let response = client
        .get(format!(
            "{}/api/perf-metrics",
            base_url.trim_end_matches('/')
        ))
        .query(&[("model", model), ("hours", &hours.to_string())])
        .send()
        .map_err(classify_transport)?;
    if !response.status().is_success() {
        return Err(classify_http(&response));
    }
    unpack(
        response
            .json::<ApiEnvelope<PerfMetricData>>()
            .map_err(classify_transport)?,
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn deserializes_log_page() {
        let json = r#"{
            "success": true,
            "data": {"page":1,"page_size":100,"total":1,"items":[{
                "id":7,"created_at":100,"type":2,"model_name":"echo",
                "channel":3,"group":"default","request_id":"req-1",
                "other":"{\"frt\":123}"
            }]}
        }"#;
        let envelope: ApiEnvelope<PageData<RemoteLog>> = serde_json::from_str(json).unwrap();
        let page = unpack(envelope).unwrap();
        assert_eq!(page.items[0].model_name, "echo");
        assert_eq!(page.items[0].log_type, 2);
    }

    #[test]
    fn sends_management_headers_and_pagination_query() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let count = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..count]).to_ascii_lowercase();
            assert!(request.contains("get /api/log/?"));
            assert!(request.contains("p=2"));
            assert!(request.contains("page_size=100"));
            assert!(request.contains("model_name=echo"));
            assert!(request.contains("authorization: test-access-token"));
            assert!(request.contains("new-api-user: 3"));
            let body = r#"{"success":true,"data":{"total":0,"items":[]}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let config = ApiConfig {
            base_url: format!("http://{address}"),
            admin_user_id: 3,
            request_timeout_secs: 5,
            ..ApiConfig::default()
        };
        let client =
            NewApiClient::with_access_token(&config, "test-access-token".to_string()).unwrap();
        let page = client.fetch_logs("echo", 10, 20, 2, 100).unwrap();
        assert_eq!(page.total, 0);
        server.join().unwrap();
    }

    #[test]
    fn classifies_business_authentication_failure() {
        let envelope: ApiEnvelope<PageData<RemoteLog>> = serde_json::from_str(
            r#"{"success":false,"message":"Unauthorized, invalid access token"}"#,
        )
        .unwrap();
        assert!(matches!(unpack(envelope), Err(ApiError::Authentication)));
    }

    #[test]
    fn classifies_http_auth_rate_limit_and_server_errors() {
        assert!(matches!(
            fetch_error_response("401 Unauthorized", ""),
            ApiError::Authentication
        ));
        assert!(matches!(
            fetch_error_response("429 Too Many Requests", "Retry-After: 7\r\n"),
            ApiError::RateLimited(Some(7))
        ));
        assert!(matches!(
            fetch_error_response("503 Service Unavailable", ""),
            ApiError::Server(StatusCode::SERVICE_UNAVAILABLE)
        ));
    }

    #[test]
    fn classifies_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            thread::sleep(Duration::from_secs(2));
        });
        let config = ApiConfig {
            base_url: format!("http://{address}"),
            admin_user_id: 3,
            request_timeout_secs: 1,
            ..ApiConfig::default()
        };
        let client = NewApiClient::with_access_token(&config, "test-access-token".to_string())
            .expect("client should build");
        assert!(matches!(
            client.fetch_logs("echo", 10, 20, 1, 1),
            Err(ApiError::Timeout)
        ));
        server.join().unwrap();
    }

    #[test]
    #[ignore = "requires an explicit live-test gate and management access token"]
    fn live_management_log_smoke() {
        assert_eq!(
            std::env::var("NEWAPI_STATUS_LIVE_TEST").as_deref(),
            Ok("1"),
            "set NEWAPI_STATUS_LIVE_TEST=1 to authorize the read-only request"
        );
        let model = std::env::var("NEWAPI_STATUS_LIVE_MODEL")
            .expect("NEWAPI_STATUS_LIVE_MODEL must name one whitelisted model");
        let config = ApiConfig {
            admin_user_id: 3,
            request_timeout_secs: 15,
            ..ApiConfig::default()
        };
        let client = NewApiClient::new(&config).expect("management client should authenticate");
        let now = chrono::Utc::now().timestamp();
        let page = client
            .fetch_logs(&model, now - 3600, now, 1, 1)
            .expect("management log query should succeed");
        assert!(page.total >= page.items.len() as i64);
    }

    fn fetch_error_response(status: &str, extra_headers: &str) -> ApiError {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let extra_headers = extra_headers.to_string();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let config = ApiConfig {
            base_url: format!("http://{address}"),
            admin_user_id: 3,
            request_timeout_secs: 5,
            ..ApiConfig::default()
        };
        let client = NewApiClient::with_access_token(&config, "test-access-token".to_string())
            .expect("client should build");
        let error = client
            .fetch_logs("echo", 10, 20, 1, 1)
            .expect_err("response should fail");
        server.join().unwrap();
        error
    }
}
