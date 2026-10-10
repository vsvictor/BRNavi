//! Controllers: map JSON-RPC methods onto use cases.

pub mod auth;
pub mod map;
pub mod shift;
pub mod system;

use std::{net::IpAddr, sync::Arc};

use map_tiles::TileService;
use serde_json::Value;
use tracing::{debug, warn};

use crate::{
    application::{AuthService, Metrics, RateLimiter, ShiftService},
    domain::RpcError,
    infrastructure::logging::sanitize_payload_for_log,
};

use super::{model::RpcRequest, view::JsonRpcView};
use auth::AuthController;
use map::MapController;
use shift::ShiftController;
use system::SystemController;

/// Per-stream context propagated for log correlation and per-client policies.
#[derive(Clone, Copy, Debug)]
pub struct StreamContext {
    pub connection_id: u64,
    pub stream_id: u64,
    /// Client address the stream belongs to (rate limiting key).
    pub remote_ip: IpAddr,
}

/// Methods after which the server closes the QUIC connection (once the reply is delivered).
/// This is the *only* application-level trigger for a server-side close; everything else is
/// left to the QUIC transport (peer close, or idle timeout = peer unreachable).
pub const CONNECTION_CLOSING_METHODS: [&str; 2] = ["logout", "rpc.logout"];

/// Methods that carry or mint credentials and are therefore rate limited per client IP.
pub const RATE_LIMITED_METHODS: [&str; 4] = ["login", "rpc.login", "rpc.resume", "createAccount"];

/// Result of handling one wire line.
#[derive(Clone, Debug, Default)]
pub struct LineOutcome {
    /// Serialized response line, or `None` for notifications.
    pub response: Option<String>,
    /// `true` when the client asked to end the session (`rpc.logout`): the transport must
    /// deliver `response` and then close the QUIC connection.
    pub close_connection: bool,
}

/// Front controller: parses the envelope, dispatches to a concrete controller, renders the view.
pub struct RpcDispatcher {
    auth: AuthController,
    shift: ShiftController,
    map: MapController,
    system: SystemController,
    rate_limiter: Arc<RateLimiter>,
    vehicles: Option<Arc<crate::application::vehicle::VehicleService>>,
    catalog_rate_limiter: RateLimiter,
}

impl RpcDispatcher {
    pub fn new(
        auth: Arc<AuthService>,
        shift: Arc<ShiftService>,
        tiles: Arc<TileService>,
        tile_max_zoom: u8,
        metrics: Arc<Metrics>,
        rate_limiter: Arc<RateLimiter>,
        session_debug_enabled: bool,
    ) -> Self {
        Self {
            auth: AuthController::new(auth, session_debug_enabled),
            shift: ShiftController::new(shift),
            map: MapController::new(tiles, Arc::clone(&metrics), tile_max_zoom),
            system: SystemController::new(metrics),
            rate_limiter,
            vehicles: None,
            catalog_rate_limiter: RateLimiter::new(
                crate::application::RateLimitConfig {
                    per_minute: 300,
                    burst: 100,
                },
                crate::application::RateLimitConfig::disabled(),
            ),
        }
    }

    pub fn with_vehicles(mut self, service: Arc<crate::application::vehicle::VehicleService>) -> Self {
        self.vehicles = Some(service);
        self
    }

    /// Handles one wire line. Returns the serialized response (or `None` for notifications) and
    /// whether the connection should be closed afterwards.
    pub async fn process_line(&self, line: &str, ctx: StreamContext) -> LineOutcome {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                let close_connection = value
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(|m| CONNECTION_CLOSING_METHODS.contains(&m));
                let response = self
                    .handle_request(value, ctx)
                    .await
                    .map(|response| JsonRpcView::to_line(&response));
                LineOutcome {
                    response,
                    close_connection,
                }
            }
            Err(err) => {
                warn!(
                    "connection_id={} stream_id={} parse error: {err}; payload={}",
                    ctx.connection_id,
                    ctx.stream_id,
                    sanitize_payload_for_log(line)
                );
                LineOutcome {
                    response: Some(JsonRpcView::to_line(&JsonRpcView::error(
                        Value::Null,
                        &RpcError::parse_error(),
                    ))),
                    close_connection: false,
                }
            }
        }
    }

    /// Handles one parsed request. Returns `None` for notifications.
    pub async fn handle_request(&self, value: Value, ctx: StreamContext) -> Option<Value> {
        let request = match RpcRequest::parse(value) {
            Ok(request) => request,
            Err((id, error)) => {
                warn!(
                    "connection_id={} stream_id={} invalid request: {error}",
                    ctx.connection_id, ctx.stream_id
                );
                return id.map(|id| JsonRpcView::error(id, &error));
            }
        };

        debug!(
            "connection_id={} stream_id={} dispatch method={} id_present={}",
            ctx.connection_id,
            ctx.stream_id,
            request.method,
            !request.is_notification()
        );

        let method = request.method.as_str();
        let params = request.params.clone();

        if method.starts_with("catalog.") {
            self.catalog_rate_limiter.cleanup(std::time::Duration::from_secs(600));
            if let Err(retry_after) = self.catalog_rate_limiter.check(ctx.remote_ip, None) {
                return request
                    .id
                    .map(|id| JsonRpcView::error(id, &RpcError::rate_limited(retry_after.as_secs().max(1))));
            }
        }

        let identity = rate_limit_identity(method, params.as_ref());
        if RATE_LIMITED_METHODS.contains(&method)
            && let Err(retry_after) = self.rate_limiter.check(ctx.remote_ip, identity.as_deref())
        {
            warn!(
                "connection_id={} stream_id={} remote_ip={} rate limited method={method} retry_after_ms={}",
                ctx.connection_id,
                ctx.stream_id,
                ctx.remote_ip,
                retry_after.as_millis()
            );
            let error = RpcError::rate_limited(retry_after.as_secs().max(1));
            return request.id.map(|id| JsonRpcView::error(id, &error));
        }

        let result = match method {
            "rpc.ping" | "rpc.health" | "echo" | "math.add" => self.system.handle(method, params).await,
            "login" | "rpc.login" | "rpc.resume" | "logout" | "rpc.logout" | "rpc.session.info"
            | "rpc.session.debug" | "createAccount" => self.auth.handle(method, params).await,
            "getShift" | "startShift" | "finishShift" => self.shift.handle(method, params).await,
            "map.getTile" => self.map.handle(method, params).await,
            "catalog.makes"
            | "catalog.models"
            | "catalog.generations"
            | "catalog.variants"
            | "catalog.license"
            | "catalog.export"
            | "garage.list"
            | "garage.add"
            | "garage.select" => match &self.vehicles {
                Some(service) => service.handle(method, params).await,
                None if method == "catalog.license" => {
                    if params
                        .as_ref()
                        .is_some_and(|p| p.as_object().is_none_or(|o| !o.is_empty()))
                    {
                        Err(RpcError::invalid_params())
                    } else {
                        Ok(crate::domain::vehicle::license())
                    }
                }
                None if method.starts_with("catalog.") => Err(crate::domain::vehicle::catalog_unavailable()),
                None => Err(crate::domain::vehicle::garage_error()),
            },
            _ => {
                warn!(
                    "connection_id={} stream_id={} method not found: {method}",
                    ctx.connection_id, ctx.stream_id
                );
                Err(RpcError::method_not_found())
            }
        };

        request.id.map(|id| JsonRpcView::render(id, result))
    }
}

/// The account-level key for the identity rate-limit tier: the phone being logged in, or the
/// session id being resumed. `None` when the params do not carry one (the request will fail
/// validation anyway, and the IP tier still applies).
fn rate_limit_identity(method: &str, params: Option<&Value>) -> Option<String> {
    if method == "createAccount" {
        return crate::domain::normalize_login(params?.get("email")?.as_str()?).ok();
    }
    if matches!(method, "login" | "rpc.login") {
        let params = params?;
        if let Some(login) = params.get("login").and_then(Value::as_str) {
            return crate::domain::normalize_login(login).ok();
        }
        return params.get("phone")?.as_str().map(str::to_string);
    }
    let field = match method {
        "rpc.resume" => "sessionId",
        _ => return None,
    };
    params?.get(field)?.as_str().map(str::to_string)
}
