//! Host wiring between the OAuth device registration and the CoopNav tracker.
//!
//! `tracking.open` turns an authenticated `(user, device)` OAuth session into a
//! server-issued tracking grant. The `CoopNavPlugin` then resolves that grant on
//! every `tracking.ingest` / `serverTime` call through [`HostSessionAuthorizer`],
//! which re-checks that the OAuth device session is still the current one.
//!
//! Namespace: personal tracking only. `driverId` is the verified token subject
//! and `brandId` is `personal:<subject>`. Mongo history stays in that partition.
//! Stage 2 may publish donors into one shared index brand; the session brand
//! returned to the client is not replaced.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use json_rpc_2_coopnav::{
    ConfirmedSession, SessionAuthorizationError, SessionAuthorizer, VerifiedPrincipal,
};
use json_rpc_2_oauth2::{AuthError, AuthPlugin, JsonRpcAuthServer, RpcError, RpcServer};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};

mod stage2;
mod storage;
pub use stage2::{Stage2Env, stage2_from_env, stage2_from_vars};
pub use storage::{
    MongoCoopNavStorage, MongoDeviceSessions, MongoTrackingGrants, MongoTrackingPermissions,
};

/// Same JSON-RPC code CoopNav uses for session authorization failures.
const DENIED_CODE: i64 = -32011;
const TRACKING_ROLE: &str = "system.xrnavi.tracking";
const STAGE1_TRACKING_PERMISSIONS: [&str; 2] = [
    json_rpc_2_coopnav::INGEST_PERMISSION,
    json_rpc_2_coopnav::SERVER_TIME_PERMISSION,
];
const STAGE2_TRACKING_PERMISSIONS: [&str; 3] = [
    json_rpc_2_coopnav::INGEST_PERMISSION,
    json_rpc_2_coopnav::SERVER_TIME_PERMISSION,
    json_rpc_2_coopnav::COOP_SYNC_PERMISSION,
];

pub fn tracking_permissions(stage2: bool) -> &'static [&'static str] {
    if stage2 {
        &STAGE2_TRACKING_PERMISSIONS
    } else {
        &STAGE1_TRACKING_PERMISSIONS
    }
}
pub const PERSONAL_BRAND_PREFIX: &str = "personal:";
/// Matches the default OAuth refresh-token lifetime used by `plugin_from_env`:
/// a device session whose `last_seen` is older can no longer be refreshed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug)]
pub struct StoreError;

#[async_trait]
pub trait TrackingPermissions: Send + Sync {
    /// Assigns only the host's fixed tracking role to the verified owner.
    async fn ensure_owner(&self, subject: &str) -> Result<(), StoreError>;
}

/// The current OAuth session of one `(user, device)` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSession {
    pub sid: String,
    pub last_seen_ms: i64,
}

#[async_trait]
pub trait DeviceSessions: Send + Sync {
    async fn current(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<DeviceSession>, StoreError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackingGrant {
    pub session_id: String,
    pub subject: String,
    pub device_id: String,
    pub auth_sid: String,
}

#[async_trait]
pub trait TrackingGrants: Send + Sync {
    /// Returns the grant already bound to `(subject, device_id, auth_sid)` or
    /// atomically stores `candidate`. Grants of older OAuth sessions of the same
    /// device may be discarded.
    async fn open(&self, candidate: TrackingGrant) -> Result<TrackingGrant, StoreError>;
    async fn find(&self, session_id: &str) -> Result<Option<TrackingGrant>, StoreError>;
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
pub struct TrackingService {
    sessions: Arc<dyn DeviceSessions>,
    grants: Arc<dyn TrackingGrants>,
    idle_timeout_ms: i64,
    clock: Clock,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    Denied,
    Unavailable,
}

impl TrackingService {
    pub fn new(sessions: Arc<dyn DeviceSessions>, grants: Arc<dyn TrackingGrants>) -> Self {
        Self::with_clock(
            sessions,
            grants,
            DEFAULT_SESSION_IDLE_TIMEOUT,
            Arc::new(|| chrono::Utc::now().timestamp_millis()),
        )
    }

    pub fn with_clock(
        sessions: Arc<dyn DeviceSessions>,
        grants: Arc<dyn TrackingGrants>,
        idle_timeout: Duration,
        clock: Clock,
    ) -> Self {
        Self {
            sessions,
            grants,
            idle_timeout_ms: i64::try_from(idle_timeout.as_millis()).unwrap_or(i64::MAX),
            clock,
        }
    }

    async fn active_session(
        &self,
        subject: &str,
        device_id: &str,
    ) -> Result<Option<DeviceSession>, StoreError> {
        let session = self.sessions.current(subject, device_id).await?;
        let now = (self.clock)();
        Ok(session.filter(|s| now.saturating_sub(s.last_seen_ms) <= self.idle_timeout_ms))
    }

    /// Issues (or returns the existing) tracking grant for a verified subject on
    /// a device the subject registered during login.
    pub async fn open(&self, subject: &str, device_id: &str) -> Result<OpenResult, OpenError> {
        let session = self
            .active_session(subject, device_id)
            .await
            .map_err(|_| OpenError::Unavailable)?
            .ok_or(OpenError::Denied)?;
        let grant = self
            .grants
            .open(TrackingGrant {
                session_id: random_session_id(),
                subject: subject.to_owned(),
                device_id: device_id.to_owned(),
                auth_sid: session.sid.clone(),
            })
            .await
            .map_err(|_| OpenError::Unavailable)?;
        if grant.subject != subject || grant.device_id != device_id || grant.auth_sid != session.sid
        {
            return Err(OpenError::Denied);
        }
        Ok(OpenResult {
            tracker_id: grant.device_id,
            driver_id: grant.subject.clone(),
            brand_id: personal_brand(&grant.subject),
            session_id: grant.session_id,
        })
    }

    pub async fn resolve(
        &self,
        subject: &str,
        session_id: &str,
    ) -> Result<ConfirmedSession, SessionAuthorizationError> {
        let unavailable = |_| SessionAuthorizationError::Unavailable;
        let grant = self
            .grants
            .find(session_id)
            .await
            .map_err(unavailable)?
            .ok_or(SessionAuthorizationError::Denied)?;
        if grant.subject != subject || grant.session_id != session_id {
            return Err(SessionAuthorizationError::Denied);
        }
        let session = self
            .active_session(&grant.subject, &grant.device_id)
            .await
            .map_err(unavailable)?
            .ok_or(SessionAuthorizationError::Denied)?;
        if session.sid != grant.auth_sid {
            return Err(SessionAuthorizationError::Denied);
        }
        ConfirmedSession::new(
            grant.session_id,
            grant.subject.clone(),
            personal_brand(&grant.subject),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct OpenResult {
    pub tracker_id: String,
    pub session_id: String,
    pub driver_id: String,
    pub brand_id: String,
}

/// `SessionAuthorizer` for `CoopNavPlugin`, backed by the persistent grants.
pub struct HostSessionAuthorizer(pub TrackingService);

#[async_trait]
impl SessionAuthorizer for HostSessionAuthorizer {
    async fn resolve(
        &self,
        principal: &VerifiedPrincipal,
        session_id: &str,
    ) -> Result<ConfirmedSession, SessionAuthorizationError> {
        self.0.resolve(&principal.subject, session_id).await
    }
}

pub fn personal_brand(subject: &str) -> String {
    format!("{PERSONAL_BRAND_PREFIX}{subject}")
}

fn random_session_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Same rule as the OAuth `DeviceInfo.deviceId` accepted at login.
fn valid_device_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenParams {
    device_id: String,
}

pub fn register<S>(
    auth: &AuthPlugin<S>,
    rpc: &RpcServer,
    service: TrackingService,
    permissions: Arc<dyn TrackingPermissions>,
    allowlist: &'static [&'static str],
) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    auth.register(rpc, "tracking.open", move |auth, params| {
        let service = service.clone();
        let permissions = permissions.clone();
        async move {
            let request = auth.authorize(params.clone(), &[])?;
            let input: OpenParams =
                serde_json::from_value(request.params).map_err(|_| RpcError::invalid_params())?;
            if !valid_device_id(&input.device_id) {
                return Err(RpcError::invalid_params());
            }
            match service
                .open(&request.claims.subject, &input.device_id)
                .await
            {
                Ok(result) => {
                    // Provision only after the subject/device ownership check.
                    permissions
                        .ensure_owner(&request.claims.subject)
                        .await
                        .map_err(|_| RpcError::internal_error())?;
                    // Role writes do not alter an already-issued token. Keep
                    // the plugin's actual permission check, including expiry.
                    let refresh_required = match auth.authorize(params, allowlist) {
                        Ok(_) => false,
                        Err(error) if error.code == -32003 => true,
                        Err(error) => return Err(error),
                    };
                    let mut response = json!({
                        "trackerId": result.tracker_id,
                        "sessionId": result.session_id,
                        "driverId": result.driver_id,
                        "brandId": result.brand_id,
                    });
                    if refresh_required {
                        response["refreshRequired"] = json!(true);
                    }
                    Ok(response)
                }
                Err(OpenError::Denied) => Err(RpcError::new(
                    DENIED_CODE,
                    "Device is not registered for this user",
                )),
                Err(OpenError::Unavailable) => Err(RpcError::internal_error()),
            }
        }
    })?;
    rpc.document_method(
        "tracking.open",
        open_schema(),
        json!({ "access_token": "PASETO v4.local access token", "deviceId": "installation-id" }),
    )?;
    Ok(())
}

fn open_schema() -> Value {
    json!({
        "type": "object",
        "description": "Opens personal tracking and provisions the owner's tracking role. Optional result.refreshRequired=true requires auth.refresh and replacing the token pair before a tracking method the token does not permit. tracking.ingest still requires only tracking.ingest. The tracking session remains valid; no repeat open is needed.",
        "additionalProperties": false,
        "required": ["deviceId"],
        "properties": {
            "access_token": { "type": "string" },
            "authorization": { "type": "string" },
            "deviceId": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9._:-]+$",
                "description": "The same deviceId sent as device.deviceId to auth.login/auth.loginWithGoogle"
            }
        }
    })
}

#[cfg(test)]
mod tests;
