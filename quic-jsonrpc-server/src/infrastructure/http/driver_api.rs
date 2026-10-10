//! reqwest-based implementation of [`DriverApiGateway`] with retry/backoff and error mapping.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use tracing::{debug, error, info, warn};

use crate::{
    domain::{DriverApiGateway, RpcError, RpcResult, codes},
    infrastructure::{config::UpstreamOptions, logging::sanitize_payload_for_log},
};

const LOGIN_PATH: &str = "/auth/login/";
const SHIFT_PATH: &str = "/driver/shift/";
const START_SHIFT_PATH: &str = "/driver/shift/start/";
const FINISH_SHIFT_PATH: &str = "/driver/shift/finish/";
const EMPTY_UPSTREAM_ERROR_MESSAGE: &str = "Upstream error response body is empty";

pub struct HttpDriverApiGateway {
    options: UpstreamOptions,
    client: Client,
}

/// One upstream call description; `label` is only used for logging.
struct UpstreamCall<'a> {
    label: &'static str,
    method: Method,
    url: String,
    bearer: Option<&'a str>,
    body: Option<Value>,
    /// Whether the call may be safely re-issued after a failure whose outcome is unknown
    /// (timeout, connection reset mid-flight, 5xx). `login` and `getShift` are; `startShift`
    /// / `finishShift` are not — a duplicate could open or close a shift twice.
    idempotent: bool,
}

/// Decides whether a failed attempt may be retried.
///
/// Idempotent calls: always. Non-idempotent calls: only when the request provably never
/// reached the upstream (connection could not be established).
fn should_retry_transport(idempotent: bool, err: &reqwest::Error) -> bool {
    idempotent || err.is_connect()
}

impl HttpDriverApiGateway {
    pub fn new(options: UpstreamOptions) -> Self {
        let client = Client::builder()
            .connect_timeout(options.timeout.min(Duration::from_secs(5)))
            .timeout(options.timeout)
            .build()
            // The builder only fails when TLS backends are misconfigured at build time; fall
            // back to the default client rather than aborting startup.
            .unwrap_or_else(|_| Client::new());
        Self { options, client }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.options.base_url, path)
    }

    fn login_url(&self) -> String {
        self.options.login_url.clone().unwrap_or_else(|| self.url(LOGIN_PATH))
    }

    async fn call(&self, call: UpstreamCall<'_>) -> RpcResult<Value> {
        let total_attempts = self.options.retries.saturating_add(1);
        let label = call.label;
        let url = &call.url;
        let backoff_ms = self.options.retry_backoff.as_millis();

        for attempt in 1..=total_attempts {
            if cfg!(debug_assertions) {
                debug!(
                    "upstream {label} attempt={attempt}/{total_attempts} {} {url} timeout_ms={} payload={}",
                    call.method,
                    self.options.timeout.as_millis(),
                    call.body
                        .as_ref()
                        .map(|b| sanitize_payload_for_log(&b.to_string()))
                        .unwrap_or_else(|| "-".into())
                );
            }

            let mut request = self
                .client
                .request(call.method.clone(), url)
                .timeout(self.options.timeout);
            if let Some(token) = call.bearer {
                request = request.bearer_auth(token);
            }
            if let Some(body) = &call.body {
                request = request.json(body);
            }

            let response = match request.send().await {
                Ok(response) => response,
                Err(err) => {
                    if attempt < total_attempts && should_retry_transport(call.idempotent, &err) {
                        warn!(
                            "upstream {label} request failed on attempt {attempt}/{total_attempts}: {err}; retrying in {backoff_ms}ms"
                        );
                        tokio::time::sleep(self.options.retry_backoff).await;
                        continue;
                    }
                    if cfg!(debug_assertions) {
                        error!("upstream {label} failed after {attempt} attempt(s): {err}");
                    } else {
                        error!("upstream {label} failed after {attempt} attempt(s)");
                    }
                    return Err(transport_error(codes::UPSTREAM_REQUEST_FAILED, &err));
                }
            };

            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|err| transport_error(codes::UPSTREAM_RESPONSE_READ_FAILED, &err))?;

            if cfg!(debug_assertions) {
                debug!(
                    "upstream {label} response attempt={attempt}/{total_attempts} status={status} body={}",
                    sanitize_payload_for_log(&body)
                );
            } else {
                info!("upstream {label} response attempt={attempt}/{total_attempts} status={status}");
            }

            if status.is_server_error() && attempt < total_attempts && call.idempotent {
                warn!(
                    "upstream {label} server error status={status} on attempt {attempt}/{total_attempts}, retrying in {backoff_ms}ms"
                );
                tokio::time::sleep(self.options.retry_backoff).await;
                continue;
            }

            if !status.is_success() {
                return Err(status_error(label, status, &body));
            }

            return Ok(parse_body(body));
        }

        Err(RpcError::owned(
            codes::UPSTREAM_REQUEST_FAILED,
            format!("Upstream {label} request failed"),
        ))
    }
}

#[async_trait]
impl DriverApiGateway for HttpDriverApiGateway {
    async fn login(&self, payload: Value) -> RpcResult<Value> {
        self.call(UpstreamCall {
            label: "login",
            method: Method::POST,
            url: self.login_url(),
            bearer: None,
            body: Some(payload),
            idempotent: true,
        })
        .await
    }

    async fn get_shift(&self, access_token: &str) -> RpcResult<Value> {
        self.call(UpstreamCall {
            label: "getShift",
            method: Method::GET,
            url: self.url(SHIFT_PATH),
            bearer: Some(access_token),
            body: None,
            idempotent: true,
        })
        .await
    }

    async fn start_shift(&self, access_token: &str, body: Value) -> RpcResult<Value> {
        self.call(UpstreamCall {
            label: "startShift",
            method: Method::POST,
            url: self.url(START_SHIFT_PATH),
            bearer: Some(access_token),
            body: Some(body),
            idempotent: false,
        })
        .await
    }

    async fn finish_shift(&self, access_token: &str, body: Value) -> RpcResult<Value> {
        self.call(UpstreamCall {
            label: "finishShift",
            method: Method::POST,
            url: self.url(FINISH_SHIFT_PATH),
            bearer: Some(access_token),
            body: Some(body),
            idempotent: false,
        })
        .await
    }
}

fn parse_body(body: String) -> Value {
    if body.trim().is_empty() {
        return Value::Null;
    }
    serde_json::from_str::<Value>(&body).unwrap_or(Value::String(body))
}

/// Maps a reqwest transport failure to an RPC error.
///
/// reqwest's `Display` includes the upstream URL (internal host/port) — that is useful in the
/// server log but must not reach the client in release builds.
fn transport_error(code: i64, err: &reqwest::Error) -> RpcError {
    if cfg!(debug_assertions) {
        let message = err.to_string();
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return RpcError::owned(code, trimmed);
        }
    }
    RpcError::new(code, "Upstream request failed")
}

fn status_error(label: &str, status: StatusCode, body: &str) -> RpcError {
    let code = RpcError::code_for_upstream_status(status.as_u16());
    if cfg!(debug_assertions) {
        warn!(
            "upstream {label} returned error status={status} mapped_code={code} body={}",
            sanitize_payload_for_log(body)
        );
    } else {
        warn!("upstream {label} returned error status={status} mapped_code={code}");
    }

    let trimmed = body.trim();
    if trimmed.is_empty() {
        return RpcError::new(code, EMPTY_UPSTREAM_ERROR_MESSAGE);
    }

    // A structured message from the upstream API is meant for the end user — relay it.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(message) = extract_message_from_json(&value)
    {
        let message = message.trim();
        if !message.is_empty() {
            return RpcError::owned(code, message);
        }
    }

    // Anything else (HTML error pages, stack traces, proxy banners) is only echoed in debug
    // builds; release clients get a neutral message.
    if cfg!(debug_assertions) {
        RpcError::owned(code, trimmed)
    } else {
        RpcError::owned(code, format!("Upstream returned HTTP {}", status.as_u16()))
    }
}

fn extract_message_from_json(value: &Value) -> Option<&str> {
    match value {
        Value::String(message) => Some(message.as_str()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| map.get("detail").and_then(Value::as_str))
            .or_else(|| map.get("title").and_then(Value::as_str))
            .or_else(|| match map.get("error") {
                Some(Value::String(message)) => Some(message.as_str()),
                Some(Value::Object(error)) => error
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("detail").and_then(Value::as_str)),
                _ => None,
            }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Produces a reqwest error whose outcome is ambiguous (timeout — the request may have
    /// reached the server). A non-routable address makes the connect attempt hang until the
    /// 1ms timeout fires instead of being refused outright.
    async fn timeout_error() -> reqwest::Error {
        let client = Client::builder().timeout(Duration::from_millis(1)).build().unwrap();
        client
            .get("http://10.255.255.1:9/")
            .send()
            .await
            .expect_err("must fail")
    }

    #[tokio::test]
    async fn non_idempotent_calls_do_not_retry_on_ambiguous_failures() {
        let err = timeout_error().await;
        if err.is_connect() {
            // Environment produced a connect error instead; that path is covered below.
            return;
        }
        assert!(!should_retry_transport(false, &err));
        assert!(should_retry_transport(true, &err));
    }

    #[tokio::test]
    async fn connect_failures_are_retried_for_everything() {
        let client = Client::new();
        let err = client
            .get("http://127.0.0.1:9/")
            .send()
            .await
            .expect_err("port 9 must be closed");
        assert!(err.is_connect(), "{err}");
        assert!(should_retry_transport(false, &err));
        assert!(should_retry_transport(true, &err));
    }

    #[test]
    fn extracts_user_facing_message() {
        assert_eq!(
            extract_message_from_json(&serde_json::json!({"error": {"detail": "bad creds"}})),
            Some("bad creds")
        );
        assert_eq!(
            extract_message_from_json(&serde_json::json!({"message": "m", "detail": "d"})),
            Some("m")
        );
        assert_eq!(extract_message_from_json(&serde_json::json!([1, 2])), None);
    }
}
