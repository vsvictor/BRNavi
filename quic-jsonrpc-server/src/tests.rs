//! Unit tests for the presentation → application → domain chain with in-memory adapters.
//!
//! The upstream driver API is replaced by [`FakeDriverApi`], which echoes the request so the
//! tests can verify what would have been sent.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use map_tiles::{Tile, TileCacheConfig, TileCoord, TileError, TileResult, TileService, TileSource};
use serde_json::{Value, json};

use crate::{
    application::{AuthService, Metrics, RateLimitConfig, RateLimiter, ShiftService},
    domain::{DriverApiGateway, RpcResult},
    infrastructure::persistence::InMemorySessionRepository,
    presentation::{RpcDispatcher, controller::StreamContext},
};

struct FakeDriverApi;

#[async_trait]
impl DriverApiGateway for FakeDriverApi {
    async fn login(&self, payload: Value) -> RpcResult<Value> {
        let phone = payload["phone"].clone();
        let device = payload["device"].clone();
        let device_id = device["deviceId"].as_str().unwrap_or("unknown");
        Ok(json!({
            "status": "ok",
            "session": {
                "tokenType": "Bearer",
                "accessToken": format!("demo-token-{device_id}"),
                "expiresInSec": 3600
            },
            "user": { "phone": phone },
            "device": device
        }))
    }

    async fn get_shift(&self, access_token: &str) -> RpcResult<Value> {
        Ok(json!({ "status": "ok", "accessToken": access_token, "shift": { "active": true } }))
    }

    async fn start_shift(&self, access_token: &str, body: Value) -> RpcResult<Value> {
        Ok(json!({ "status": "ok", "accessToken": access_token, "request": body, "shift": { "started": true } }))
    }

    async fn finish_shift(&self, access_token: &str, body: Value) -> RpcResult<Value> {
        Ok(json!({ "status": "ok", "accessToken": access_token, "request": body, "shift": { "finished": true } }))
    }
}

/// Returns a tiny deterministic "PNG" for any coord except `z=9,x=9,y=9`, which is a 404.
struct FakeTileSource;

const TILE_MAX_ZOOM: u8 = 19;

#[async_trait]
impl TileSource for FakeTileSource {
    async fn fetch(&self, coord: TileCoord) -> TileResult<Tile> {
        if coord == TileCoord::new(9, 9, 9, TILE_MAX_ZOOM).unwrap() {
            return Err(TileError::NotFound);
        }
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(coord.to_string().as_bytes());
        Ok(Tile {
            content_type: "image/png".into(),
            bytes,
            etag: Some(format!("\"etag-{coord}\"")),
        })
    }
}

fn tile_service() -> Arc<TileService> {
    Arc::new(TileService::new(
        Arc::new(FakeTileSource),
        TileCacheConfig {
            max_bytes: 1 << 20,
            ttl: Duration::from_secs(60),
        },
    ))
}

pub(crate) fn dispatcher_with_auth(auth: Arc<AuthService>) -> RpcDispatcher {
    RpcDispatcher::new(
        auth,
        Arc::new(ShiftService::new(Arc::new(FakeDriverApi))),
        tile_service(),
        TILE_MAX_ZOOM,
        Arc::new(Metrics::new()),
        Arc::new(RateLimiter::new(
            RateLimitConfig::disabled(),
            RateLimitConfig::disabled(),
        )),
        false,
    )
}

fn dispatcher_with(ttl: Duration, session_debug_enabled: bool) -> RpcDispatcher {
    dispatcher_with_limits(
        ttl,
        session_debug_enabled,
        RateLimitConfig::disabled(),
        RateLimitConfig::disabled(),
    )
}

fn dispatcher_with_limit(ttl: Duration, session_debug_enabled: bool, per_ip: RateLimitConfig) -> RpcDispatcher {
    dispatcher_with_limits(ttl, session_debug_enabled, per_ip, RateLimitConfig::disabled())
}

fn dispatcher_with_limits(
    ttl: Duration,
    session_debug_enabled: bool,
    per_ip: RateLimitConfig,
    per_identity: RateLimitConfig,
) -> RpcDispatcher {
    let sessions = Arc::new(InMemorySessionRepository::new(ttl));
    let gateway: Arc<dyn DriverApiGateway> = Arc::new(FakeDriverApi);
    let auth = Arc::new(AuthService::new(sessions, gateway.clone(), ttl));
    let shift = Arc::new(ShiftService::new(gateway));
    let metrics = Arc::new(Metrics::new());
    RpcDispatcher::new(
        auth,
        shift,
        tile_service(),
        TILE_MAX_ZOOM,
        metrics,
        Arc::new(RateLimiter::new(per_ip, per_identity)),
        session_debug_enabled,
    )
}

fn dispatcher() -> RpcDispatcher {
    dispatcher_with(Duration::from_secs(3600), false)
}

const CTX: StreamContext = StreamContext {
    connection_id: 1,
    stream_id: 1,
    remote_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
};

async fn call(d: &RpcDispatcher, request: Value) -> Option<Value> {
    d.handle_request(request, CTX).await
}

fn login_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "login",
        "id": id,
        "params": {
            "phone": "+380991112233",
            "password": "secret",
            "device": {
                "platform": "android",
                "appVersion": "1.2.3",
                "deviceId": "device-123",
                "model": "Pixel 8",
                "osVersion": "14"
            }
        }
    })
}

async fn login_and_get_session_id(d: &RpcDispatcher) -> String {
    let response = call(d, login_request(1)).await.expect("login response");
    response["result"]["session"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string()
}

#[tokio::test]
async fn ping_request_returns_pong() {
    let d = dispatcher();
    let response = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 1}))
        .await
        .expect("response expected");
    assert_eq!(response["result"], json!("pong"));
    assert_eq!(response["id"], json!(1));
}

#[tokio::test]
async fn notification_returns_no_response() {
    let d = dispatcher();
    assert!(
        call(&d, json!({"jsonrpc": "2.0", "method": "rpc.ping"}))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn invalid_version_with_id_returns_invalid_request() {
    let d = dispatcher();
    let response = call(&d, json!({"jsonrpc": "1.0", "method": "rpc.ping", "id": 5}))
        .await
        .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32600));
    assert_eq!(response["id"], json!(5));
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let d = dispatcher();
    let response = call(&d, json!({"jsonrpc": "2.0", "method": "nope", "id": 1}))
        .await
        .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32601));
}

#[tokio::test]
async fn session_debug_is_hidden_when_disabled() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.session.debug", "id": 999, "params": {"sessionId": "sess-unknown"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32601));
}

#[tokio::test]
async fn parse_error_is_returned() {
    let d = dispatcher();
    let outcome = d.process_line("{not-json", CTX).await;
    let response = outcome.response.expect("parse error response");
    let parsed: Value = serde_json::from_str(&response).expect("valid json response");
    assert_eq!(parsed["error"]["code"], json!(-32700));
    assert!(!outcome.close_connection, "parse errors must not close the connection");
}

#[tokio::test]
async fn regular_requests_do_not_close_connection() {
    let d = dispatcher();
    let outcome = d
        .process_line(r#"{"jsonrpc":"2.0","method":"rpc.ping","id":1}"#, CTX)
        .await;
    assert!(outcome.response.is_some());
    assert!(!outcome.close_connection);
}

#[tokio::test]
async fn logout_removes_session_and_requests_connection_close() {
    let d = dispatcher();
    let session_id = login_and_get_session_id(&d).await;

    let line =
        json!({"jsonrpc": "2.0", "method": "rpc.logout", "id": 800, "params": {"sessionId": session_id}}).to_string();
    let outcome = d.process_line(&line, CTX).await;
    assert!(outcome.close_connection, "logout must close the QUIC connection");
    let response: Value = serde_json::from_str(&outcome.response.expect("logout response")).expect("json");
    assert_eq!(response["result"]["loggedOut"], json!(true));
    assert_eq!(response["result"]["sessionRemoved"], json!(true));

    // The session is gone: resume must fail.
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 801, "params": {"sessionId": session_id}}),
    )
    .await
    .expect("resume response");
    assert_eq!(response["error"]["code"], json!(-32001));
}

#[tokio::test]
async fn logout_without_session_is_ok_and_closes_connection() {
    let d = dispatcher();

    let outcome = d
        .process_line(r#"{"jsonrpc":"2.0","method":"logout","id":1}"#, CTX)
        .await;
    assert!(outcome.close_connection);
    let response: Value = serde_json::from_str(&outcome.response.expect("response")).expect("json");
    assert_eq!(response["result"]["loggedOut"], json!(true));
    assert_eq!(response["result"]["sessionRemoved"], json!(false));

    // Unknown session id: idempotent, still fine.
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.logout", "id": 2, "params": {"sessionId": "nope"}}),
    )
    .await
    .expect("response");
    assert_eq!(response["result"]["sessionRemoved"], json!(false));
}

#[tokio::test]
async fn logout_as_notification_still_closes_connection() {
    let d = dispatcher();
    let outcome = d.process_line(r#"{"jsonrpc":"2.0","method":"rpc.logout"}"#, CTX).await;
    assert!(outcome.response.is_none());
    assert!(outcome.close_connection);
}

#[tokio::test]
async fn login_is_rate_limited_per_ip_but_ping_is_not() {
    let d = dispatcher_with_limit(
        Duration::from_secs(3600),
        false,
        RateLimitConfig {
            per_minute: 1,
            burst: 2,
        },
    );

    assert!(call(&d, login_request(1)).await.unwrap().get("error").is_none());
    assert!(call(&d, login_request(2)).await.unwrap().get("error").is_none());
    let third = call(&d, login_request(3)).await.unwrap();
    assert_eq!(third["error"]["code"], json!(-32004));

    // Other methods are unaffected …
    let pong = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 4}))
        .await
        .unwrap();
    assert_eq!(pong["result"], json!("pong"));

    // … and so is another client.
    let other = StreamContext {
        remote_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7)),
        ..CTX
    };
    let response = d.handle_request(login_request(5), other).await.unwrap();
    assert!(response.get("error").is_none(), "{response}");
}

#[tokio::test]
async fn login_is_rate_limited_per_phone_across_ips() {
    let d = dispatcher_with_limits(
        Duration::from_secs(3600),
        false,
        RateLimitConfig::disabled(),
        RateLimitConfig {
            per_minute: 1,
            burst: 1,
        },
    );

    assert!(call(&d, login_request(1)).await.unwrap().get("error").is_none());
    // Same phone from a different IP → throttled.
    let other = StreamContext {
        remote_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7)),
        ..CTX
    };
    let second = d.handle_request(login_request(2), other).await.unwrap();
    assert_eq!(second["error"]["code"], json!(-32004));

    // Another driver behind the same NAT is unaffected.
    let mut other_phone = login_request(3);
    other_phone["params"]["phone"] = json!("+380997770000");
    assert!(call(&d, other_phone).await.unwrap().get("error").is_none());
}

#[tokio::test]
async fn resume_and_info_report_configured_session_ttl() {
    let d = dispatcher_with(Duration::from_secs(1234), false);
    let session_id = login_and_get_session_id(&d).await;
    for method in ["rpc.resume", "rpc.session.info"] {
        let response = call(
            &d,
            json!({"jsonrpc": "2.0", "method": method, "id": 1, "params": {"sessionId": session_id}}),
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["session"]["expiresInSec"], json!(1234), "{method}");
    }
}

#[tokio::test]
async fn request_id_must_be_string_number_or_null() {
    let d = dispatcher();
    for bad_id in [json!({"a": 1}), json!([1]), json!(true)] {
        let response = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": bad_id}))
            .await
            .expect("invalid request must be answered");
        assert_eq!(response["error"]["code"], json!(-32600));
        assert!(response["id"].is_null());
    }
    for good_id in [json!("s"), json!(7), json!(1.5), Value::Null] {
        let response = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": good_id}))
            .await
            .expect("valid id must be answered");
        assert_eq!(response["result"], json!("pong"));
        assert_eq!(response["id"], good_id);
    }
}

#[tokio::test]
async fn add_supports_object_and_array_params() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "math.add", "params": {"a": 1.25, "b": 2.75}, "id": "sum-1"}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["result"], json!(4.0));

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "math.add", "params": [1, 2], "id": "sum-2"}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["result"], json!(3.0));
}

#[tokio::test]
async fn health_returns_metrics_payload() {
    let d = dispatcher();
    let response = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 7}))
        .await
        .expect("response expected");
    assert!(response["result"]["connections"].is_object());
    assert!(response["result"]["rpc"].is_object());
}

#[tokio::test]
async fn login_accepts_expected_payload() {
    let d = dispatcher();
    let response = call(&d, login_request(55)).await.expect("response expected");
    assert_eq!(response["result"]["status"], json!("ok"));
    assert_eq!(response["result"]["user"]["phone"], json!("+380991112233"));
    assert_eq!(response["result"]["session"]["tokenType"], json!("Bearer"));
    assert!(response["result"]["session"]["sessionId"].is_string());
}

#[tokio::test]
async fn login_accepts_email_and_md5_password_and_rejects_plaintext() {
    let d = dispatcher();
    let mut request = json!({
        "jsonrpc": "2.0", "method": "login", "id": 57,
        "params": {"login": "driver@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"}
    });
    let response = call(&d, request.clone()).await.unwrap();
    assert_eq!(response["result"]["status"], json!("ok"));
    assert!(response["result"]["session"]["sessionId"].is_string());
    request["params"]["password"] = json!("plaintext");
    let response = call(&d, request).await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32602));
}

#[test]
fn email_login_forwards_exact_payload_without_phone_or_device() {
    let payload = json!({"login": "driver@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"});
    let command = crate::presentation::model::LoginParams::parse(Some(payload.clone())).unwrap();
    assert_eq!(command.upstream_payload, payload);
    assert_eq!(command.principal.phone, "driver@example.com");
    assert_eq!(command.principal.device_id, "email-login");
}

#[tokio::test]
async fn email_login_is_rate_limited_across_ips() {
    let d = dispatcher_with_limits(
        Duration::from_secs(3600),
        false,
        RateLimitConfig::disabled(),
        RateLimitConfig {
            per_minute: 1,
            burst: 1,
        },
    );
    let request = json!({"jsonrpc": "2.0", "method": "login", "id": 58,
        "params": {"login": "driver@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"}});
    assert!(call(&d, request.clone()).await.unwrap().get("error").is_none());
    let other = StreamContext {
        remote_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 8)),
        ..CTX
    };
    assert_eq!(
        d.handle_request(request, other).await.unwrap()["error"]["code"],
        json!(-32004)
    );
}

#[tokio::test]
async fn email_login_case_and_whitespace_cannot_bypass_identity_rate_limit() {
    let d = dispatcher_with_limits(
        Duration::from_secs(60),
        false,
        RateLimitConfig::disabled(),
        RateLimitConfig {
            per_minute: 1,
            burst: 1,
        },
    );
    let request = json!({"jsonrpc": "2.0", "method": "login", "id": 1,
        "params": {"login": "driver@example.com", "password": "5f4dcc3b5aa765d61d8327deb882cf99"}});
    assert!(call(&d, request.clone()).await.unwrap().get("error").is_none());
    let mut variant = request;
    variant["params"]["login"] = json!(" DRIVER@Example.COM ");
    let response = d
        .handle_request(
            variant,
            StreamContext {
                remote_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 9)),
                ..CTX
            },
        )
        .await
        .unwrap();
    assert_eq!(response["error"]["code"], json!(-32004));
}

#[tokio::test]
async fn login_rejects_invalid_payload() {
    let d = dispatcher();
    let mut request = login_request(56);
    request["params"]["device"].as_object_mut().unwrap().remove("deviceId");
    let response = call(&d, request).await.expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32602));
}

#[tokio::test]
async fn login_returns_same_session_id_for_same_principal() {
    let d = dispatcher();
    let first = call(&d, login_request(200)).await.expect("first response");
    let second = call(&d, login_request(201)).await.expect("second response");
    assert_eq!(
        first["result"]["session"]["sessionId"],
        second["result"]["session"]["sessionId"]
    );
}

#[tokio::test]
async fn resume_returns_existing_session() {
    let d = dispatcher();
    let session_id = login_and_get_session_id(&d).await;

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 202, "params": {"sessionId": session_id}}),
    )
    .await
    .expect("resume response");
    assert_eq!(response["result"]["status"], json!("ok"));
    assert_eq!(response["result"]["resumed"], json!(true));
    assert_eq!(
        response["result"]["session"]["accessToken"],
        json!("demo-token-device-123")
    );
}

#[tokio::test]
async fn resume_fails_after_session_expiration() {
    let d = dispatcher_with(Duration::from_millis(1), false);
    let session_id = login_and_get_session_id(&d).await;

    std::thread::sleep(Duration::from_millis(5));

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 301, "params": {"sessionId": session_id}}),
    )
    .await
    .expect("resume response");
    assert_eq!(response["error"]["code"], json!(-32001));
}

#[tokio::test]
async fn session_info_hides_access_token() {
    let d = dispatcher();
    let session_id = login_and_get_session_id(&d).await;

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.session.info", "id": 401, "params": {"sessionId": session_id}}),
    )
    .await
    .expect("session info response");
    assert_eq!(response["result"]["status"], json!("ok"));
    assert_eq!(response["result"]["user"]["phone"], json!("+380991112233"));
    assert_eq!(response["result"]["device"]["deviceId"], json!("device-123"));
    assert!(response["result"]["session"]["accessToken"].is_null());
}

#[tokio::test]
async fn session_debug_returns_access_token_when_enabled() {
    let d = dispatcher_with(Duration::from_secs(3600), true);
    let session_id = login_and_get_session_id(&d).await;

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.session.debug", "id": 501, "params": {"sessionId": session_id}}),
    )
    .await
    .expect("session debug response");
    assert_eq!(response["result"]["debug"], json!(true));
    assert!(response["result"]["session"]["accessToken"].is_string());
}

#[tokio::test]
async fn get_shift_requires_access_token() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "getShift", "id": 1, "params": {}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32602));

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "getShift", "id": 2, "params": {"accessToken": "token-123"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["result"]["shift"]["active"], json!(true));
}

#[tokio::test]
async fn start_shift_forwards_contract_and_returns_result() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 600,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:12Z",
                "sendTime": 1726395072,
                "vehicleId": "vehicle-123",
                "lat": 50.45
            }
        }),
    )
    .await
    .expect("response expected");

    assert_eq!(response["result"]["status"], json!("ok"));
    assert_eq!(response["result"]["accessToken"], json!("token-123"));
    assert_eq!(
        response["result"]["request"]["deviceTime"],
        json!("2026-09-15T10:11:12Z")
    );
    assert_eq!(response["result"]["request"]["sendTime"], json!(1726395072));
    assert_eq!(response["result"]["request"]["vehicleId"], json!("vehicle-123"));
    assert_eq!(response["result"]["request"]["lat"], json!(50.45));
    assert!(response["result"]["request"]["lng"].is_null());
    assert!(response["result"]["request"]["accessToken"].is_null());
}

#[tokio::test]
async fn start_shift_rejects_missing_required_fields() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "startShift", "id": 601, "params": {"accessToken": "token-123", "deviceTime": "t", "sendTime": "t"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32602));
}

#[tokio::test]
async fn finish_shift_accepts_payload_and_returns_result() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "finishShift", "id": 700, "params": {"accessToken": "token-123", "deviceTime": "2026-09-15T10:11:20Z"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["result"]["status"], json!("ok"));
    assert_eq!(
        response["result"]["request"]["deviceTime"],
        json!("2026-09-15T10:11:20Z")
    );
    assert!(response["result"]["request"]["accessToken"].is_null());
}

#[tokio::test]
async fn finish_shift_rejects_missing_fields() {
    let d = dispatcher();
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "finishShift", "id": 701, "params": {"accessToken": "token-123"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32602));

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "finishShift", "id": 702, "params": {"deviceTime": "t"}}),
    )
    .await
    .expect("response expected");
    assert_eq!(response["error"]["code"], json!(-32602));
}

/// Upstream that issues its own (predictable) `session.sessionId`.
struct UpstreamWithSessionId;

#[async_trait]
impl DriverApiGateway for UpstreamWithSessionId {
    async fn login(&self, _payload: Value) -> RpcResult<Value> {
        Ok(json!({
            "status": "ok",
            "session": { "sessionId": "upstream-1", "accessToken": "tok" }
        }))
    }
    async fn get_shift(&self, _: &str) -> RpcResult<Value> {
        Ok(Value::Null)
    }
    async fn start_shift(&self, _: &str, _: Value) -> RpcResult<Value> {
        Ok(Value::Null)
    }
    async fn finish_shift(&self, _: &str, _: Value) -> RpcResult<Value> {
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn login_never_uses_upstream_session_id_as_credential() {
    let sessions = Arc::new(InMemorySessionRepository::new(Duration::from_secs(60)));
    let gateway: Arc<dyn DriverApiGateway> = Arc::new(UpstreamWithSessionId);
    let auth = Arc::new(AuthService::new(sessions, gateway.clone(), Duration::from_secs(60)));
    let metrics = Arc::new(Metrics::new());
    let d = RpcDispatcher::new(
        auth,
        Arc::new(ShiftService::new(gateway)),
        tile_service(),
        TILE_MAX_ZOOM,
        metrics,
        Arc::new(RateLimiter::new(
            RateLimitConfig::disabled(),
            RateLimitConfig::disabled(),
        )),
        false,
    );

    let response = call(&d, login_request(900)).await.expect("login response");
    let session = &response["result"]["session"];
    let our_id = session["sessionId"].as_str().expect("sessionId");
    assert!(our_id.starts_with("sess-"), "must be minted locally, got {our_id}");
    assert_eq!(session["upstreamSessionId"], json!("upstream-1"));

    // The upstream id must not be resumable.
    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 901, "params": {"sessionId": "upstream-1"}}),
    )
    .await
    .expect("resume response");
    assert!(response["error"].is_object(), "upstream id must not resume: {response}");

    let response = call(
        &d,
        json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 902, "params": {"sessionId": our_id}}),
    )
    .await
    .expect("resume response");
    assert_eq!(response["result"]["session"]["accessToken"], json!("tok"));
}

#[tokio::test]
async fn different_principals_get_different_session_ids() {
    let d = dispatcher();
    let first = login_and_get_session_id(&d).await;
    let mut other = login_request(2);
    other["params"]["device"]["deviceId"] = json!("device-456");
    let second = call(&d, other).await.expect("login response");
    assert_ne!(first, second["result"]["session"]["sessionId"].as_str().unwrap());
}

fn tile_request(id: u64, z: u64, x: u64, y: u64) -> Value {
    json!({"jsonrpc": "2.0", "method": "map.getTile", "id": id, "params": {"z": z, "x": x, "y": y}})
}

#[tokio::test]
async fn get_tile_returns_base64_png_and_caches() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    let d = dispatcher();
    let first = call(&d, tile_request(1, 12, 2400, 1400)).await.expect("tile response");
    let result = &first["result"];
    assert_eq!(result["z"], json!(12));
    assert_eq!(result["x"], json!(2400));
    assert_eq!(result["y"], json!(1400));
    assert_eq!(result["contentType"], json!("image/png"));
    assert_eq!(result["etag"], json!("\"etag-12/2400/1400\""));
    assert_eq!(result["fromCache"], json!(false));

    let bytes = BASE64
        .decode(result["data"].as_str().expect("base64 data"))
        .expect("valid base64");
    assert!(bytes.starts_with(b"\x89PNG"));
    assert_eq!(result["size"], json!(bytes.len()));

    let second = call(&d, tile_request(2, 12, 2400, 1400)).await.expect("tile response");
    assert_eq!(second["result"]["fromCache"], json!(true));
    assert_eq!(second["result"]["data"], result["data"]);

    let health = call(&d, json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 3}))
        .await
        .unwrap();
    assert_eq!(health["result"]["tiles"]["cache_hits"], json!(1));
    assert_eq!(health["result"]["tiles"]["cache_misses"], json!(1));
}

#[tokio::test]
async fn get_tile_validates_coordinates() {
    let d = dispatcher();
    // zoom above the limit
    let response = call(&d, tile_request(1, 20, 0, 0)).await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32602));
    // x out of range for zoom
    let response = call(&d, tile_request(2, 3, 8, 0)).await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32602));
    // negative / non-integer / missing
    for params in [
        json!({"z": -1, "x": 0, "y": 0}),
        json!({"z": 1.5, "x": 0, "y": 0}),
        json!({"z": 1, "x": "0", "y": 0}),
        json!({"z": 1, "x": 0}),
        json!([1, 0, 0]),
    ] {
        let response = call(
            &d,
            json!({"jsonrpc": "2.0", "method": "map.getTile", "id": 9, "params": params}),
        )
        .await
        .unwrap();
        assert_eq!(response["error"]["code"], json!(-32602), "{params}");
    }
}

#[tokio::test]
async fn get_tile_propagates_upstream_errors() {
    let d = dispatcher();
    let response = call(&d, tile_request(1, 9, 9, 9)).await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32010));
    assert_eq!(response["error"]["message"], json!("Tile not found"));
}

#[tokio::test]
async fn get_tile_is_not_rate_limited_and_does_not_close_connection() {
    let d = dispatcher_with_limit(
        Duration::from_secs(3600),
        false,
        RateLimitConfig {
            per_minute: 1,
            burst: 1,
        },
    );
    for i in 0..5u64 {
        let outcome = d.process_line(&tile_request(i, 5, i, i).to_string(), CTX).await;
        assert!(!outcome.close_connection);
        let response: Value = serde_json::from_str(&outcome.response.unwrap()).unwrap();
        assert!(response.get("error").is_none(), "{response}");
    }
}

fn registration_request(email: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "createAccount", "id": 1, "params": {
        "name": "Test Driver", "email": email, "password": "5f4dcc3b5aa765d61d8327deb882cf99"
    }})
}

#[tokio::test]
async fn create_account_validates_params_and_routes_without_http_fallback() {
    let d = dispatcher();
    let valid = registration_request(" TEST@Example.com ");
    let response = call(&d, valid.clone()).await.unwrap();
    // The legacy HTTP gateway has no registration method and must not receive credentials.
    assert_eq!(response["error"]["code"], -32601);
    for (field, value) in [
        ("name", json!("")),
        ("name", json!("Single")),
        ("name", json!("First\nLast")),
        ("name", json!(format!("{} Last", "A".repeat(100)))),
        ("name", json!(22)),
        ("email", json!("invalid")),
        ("password", json!("not-md5")),
        ("password", json!("5F4DCC3B5AA765D61D8327DEB882CF99")),
        ("password", json!("5f4dcc3b5aa765d61d8327deb882cf9z")),
    ] {
        let mut invalid = valid.clone();
        invalid["params"][field] = value;
        assert_eq!(call(&d, invalid).await.unwrap()["error"]["code"], -32602);
    }
    for params in [
        Value::Null,
        json!([]),
        json!({}),
        json!({
            "name": "Test Driver", "email": "test@example.com",
            "password": "5f4dcc3b5aa765d61d8327deb882cf99", "enabled": true
        }),
    ] {
        let mut invalid = valid.clone();
        invalid["params"] = params;
        assert_eq!(call(&d, invalid).await.unwrap()["error"]["code"], -32602);
    }
}

#[tokio::test]
async fn create_account_rate_limit_uses_normalized_email_across_ips_and_ip_for_invalid_params() {
    let one = RateLimitConfig {
        per_minute: 1,
        burst: 1,
    };
    let d = dispatcher_with_limits(Duration::from_secs(60), false, RateLimitConfig::disabled(), one);
    assert_eq!(
        call(&d, registration_request(" TEST@Example.com ")).await.unwrap()["error"]["code"],
        -32601
    );
    let other = StreamContext {
        remote_ip: "192.0.2.1".parse().unwrap(),
        ..CTX
    };
    assert_eq!(
        d.handle_request(registration_request("test@example.com"), other)
            .await
            .unwrap()["error"]["code"],
        -32004
    );
    let d = dispatcher_with_limit(Duration::from_secs(60), false, one);
    assert_eq!(
        call(&d, registration_request("invalid")).await.unwrap()["error"]["code"],
        -32602
    );
    assert_eq!(
        call(&d, registration_request("other@example.com")).await.unwrap()["error"]["code"],
        -32004
    );
}

#[tokio::test]
async fn public_catalog_is_rate_limited_without_affecting_ping_or_auth() {
    let d = dispatcher();
    let request = json!({"jsonrpc":"2.0","method":"catalog.license","id":1,"params":{}});
    for _ in 0..100 {
        let response = call(&d, request.clone()).await.unwrap();
        assert_eq!(response["result"]["name"], "ODbL-1.0");
        assert_eq!(response["result"]["export"]["authenticationRequired"], false);
    }
    assert_eq!(call(&d, request.clone()).await.unwrap()["error"]["code"], -32004);
    let other = StreamContext {
        remote_ip: "192.0.2.99".parse().unwrap(),
        ..CTX
    };
    assert!(d.handle_request(request, other).await.unwrap().get("result").is_some());
    assert!(
        call(&d, json!({"jsonrpc":"2.0","method":"rpc.ping","id":2}))
            .await
            .unwrap()
            .get("result")
            .is_some()
    );
    assert!(call(&d, login_request(3)).await.unwrap().get("result").is_some());
}
