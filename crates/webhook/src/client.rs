//! Webhook HTTP client ().
//!
//! ```http
//! POST /api/v1/external/hammerspoon
//! Content-Type: application/json
//! ```
//! ```json
//! {
//!   "app_name": "WhatsApp",
//!   "title": "Niles 💟",
//!   "message": "AAA",
//!   "notification_id": "79BAE16D-…",
//!   "timestamp": "2026-09-09T02:30:00+07:00"
//! }
//! ```

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use tracing::{info, warn};

use crate::retry::{backoff_for_attempt, should_retry, should_retry_error};

/// JSON payload posted to the webhook.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub app_name: String,
    pub title: String,
    pub message: String,
    /// Maps from the internal `Notification.id`; `None` serializes as null.
    pub notification_id: Option<String>,
    /// RFC 3339 with local offset, e.g. `2026-09-09T02:30:00+07:00`.
    pub timestamp: String,
}

impl WebhookPayload {
    pub fn test() -> Self {
        Self {
            app_name: "Notification Forwarder".to_string(),
            title: "Test".to_string(),
            message: "Webhook test from Notification Forwarder".to_string(),
            notification_id: None,
            timestamp: chrono::Local::now().to_rfc3339(),
        }
    }
}

#[derive(Debug)]
pub enum WebhookError {
    Configuration(String),
    Transport(reqwest::Error),
    Status(u16),
}

impl std::fmt::Display for WebhookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(e) => write!(f, "invalid webhook configuration: {e}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Status(s) => write!(f, "unexpected status: {s}"),
        }
    }
}

impl std::error::Error for WebhookError {}

/// Thin wrapper over `reqwest` with  retry semantics.
#[derive(Debug, Clone)]
pub struct WebhookClient {
    url: String,
    inner: reqwest::Client,
    max_attempts: u32,
    configuration_error: Option<String>,
}

impl WebhookClient {
    pub fn new(
        url: impl Into<String>,
        max_attempts: u32,
        custom_headers: &BTreeMap<String, String>,
    ) -> Self {
        let (headers, configuration_error) = match parse_headers(custom_headers) {
            Ok(headers) => (headers, None),
            Err(error) => (HeaderMap::new(), Some(error)),
        };
        Self {
            url: url.into(),
            inner: reqwest::Client::builder()
                .default_headers(headers)
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client builds"),
            max_attempts: max_attempts.max(1).min(5),
            configuration_error,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// POST with retry. Returns the final status on success (2xx).
    pub async fn send(&self, payload: &WebhookPayload) -> Result<u16, WebhookError> {
        if let Some(error) = &self.configuration_error {
            return Err(WebhookError::Configuration(error.clone()));
        }
        let mut last_error = None;
        for attempt in 1..=self.max_attempts {
            if attempt > 1 {
                let backoff = backoff_for_attempt(attempt);
                info!(attempt, delay_ms = backoff.as_millis(), "webhook retry");
                tokio::time::sleep(backoff).await;
            }
            match self.try_once(payload).await {
                Ok(status) if (200..300).contains(&status) => {
                    info!(status, attempt, "webhook response");
                    return Ok(status);
                }
                Ok(status) if should_retry(status) && attempt < self.max_attempts => {
                    warn!(status, attempt, "transient webhook status; will retry");
                    last_error = Some(WebhookError::Status(status));
                    continue;
                }
                Ok(status) => return Err(WebhookError::Status(status)),
                Err(e) if e.is_transport_retryable() && attempt < self.max_attempts => {
                    warn!(error = %e, attempt, "transient webhook error; will retry");
                    last_error = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_error.expect("at least one attempt ran"))
    }

    async fn try_once(&self, payload: &WebhookPayload) -> Result<u16, WebhookError> {
        info!("webhook request started");
        let response = self
            .inner
            .post(&self.url)
            .json(payload)
            .send()
            .await
            .map_err(|e| WebhookError::Transport(e))?;
        Ok(response.status().as_u16())
    }
}

trait TransportRetryable {
    fn is_transport_retryable(&self) -> bool;
}

impl TransportRetryable for WebhookError {
    fn is_transport_retryable(&self) -> bool {
        match self {
            Self::Configuration(_) => false,
            Self::Transport(e) => should_retry_error(e),
            Self::Status(s) => should_retry(*s),
        }
    }
}

/// Validate custom header names and values without exposing their contents.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), String> {
    parse_headers(headers).map(|_| ())
}

fn parse_headers(headers: &BTreeMap<String, String>) -> Result<HeaderMap, String> {
    let mut parsed = HeaderMap::new();
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid header name `{name}`"))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|_| format!("invalid value for header `{name}`"))?;
        parsed.insert(header_name, header_value);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_custom_headers() {
        let headers = BTreeMap::from([
            ("Authorization".to_string(), "Bearer token".to_string()),
            ("X-Source".to_string(), "macOS".to_string()),
        ]);
        assert!(validate_headers(&headers).is_ok());
    }

    #[test]
    fn rejects_invalid_header_names_and_values() {
        let bad_name = BTreeMap::from([("Bad Header".to_string(), "value".to_string())]);
        assert!(validate_headers(&bad_name).is_err());

        let bad_value = BTreeMap::from([("X-Test".to_string(), "one\ntwo".to_string())]);
        assert!(validate_headers(&bad_value).is_err());
    }

    #[tokio::test]
    async fn sends_custom_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.contains("\r\nauthorization: bearer test-token\r\n"));
            assert!(request.contains("\r\nx-source: notification-forwarder\r\n"));
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let headers = BTreeMap::from([
            ("Authorization".to_string(), "Bearer test-token".to_string()),
            ("X-Source".to_string(), "notification-forwarder".to_string()),
        ]);
        let client = WebhookClient::new(format!("http://{address}"), 1, &headers);
        assert_eq!(client.send(&WebhookPayload::test()).await.unwrap(), 204);
        server.await.unwrap();
    }
}
