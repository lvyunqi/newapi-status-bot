use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::config::{ApiConfig, PerfMetricsConfig};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("缺少环境变量 {0}")]
    MissingToken(String),
    #[error("HTTP 请求失败: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("New API 返回 HTTP {0}")]
    Http(StatusCode),
    #[error("New API 业务错误: {0}")]
    Business(String),
    #[error("New API 响应缺少 data")]
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

    fn with_access_token(config: &ApiConfig, access_token: String) -> Result<Self, ApiError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5.min(config.request_timeout_secs)))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .user_agent("newapi-status-bot/0.1")
            .build()?;
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
            .bearer_auth(&self.access_token)
            .header("New-Api-User", self.admin_user_id.to_string())
            .query(&[
                ("p", page.to_string()),
                ("page_size", page_size.to_string()),
                ("start_timestamp", start_timestamp.to_string()),
                ("end_timestamp", end_timestamp.to_string()),
                ("model_name", model.to_string()),
            ])
            .send()?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status()));
        }
        unpack(response.json::<ApiEnvelope<PageData<RemoteLog>>>()?)
    }
}

fn unpack<T>(envelope: ApiEnvelope<T>) -> Result<T, ApiError> {
    if !envelope.success {
        return Err(ApiError::Business(
            envelope.message.unwrap_or_else(|| "未知错误".to_string()),
        ));
    }
    envelope.data.ok_or(ApiError::MissingData)
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
        .build()?;
    let response = client
        .get(format!(
            "{}/api/perf-metrics",
            base_url.trim_end_matches('/')
        ))
        .query(&[("model", model), ("hours", &hours.to_string())])
        .send()?;
    if !response.status().is_success() {
        return Err(ApiError::Http(response.status()));
    }
    unpack(response.json::<ApiEnvelope<PerfMetricData>>()?)
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
            assert!(request.contains("authorization: bearer test-access-token"));
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
}
