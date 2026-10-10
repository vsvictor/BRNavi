//! Authentication & session use cases: login, resume, info, debug.

use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::domain::{
    Authenticator, DriverApiGateway, GatewayAuthenticator, Principal, RpcError, RpcResult, Session, SessionRepository,
};

/// Validated login input. Built by the presentation layer from the raw JSON-RPC params.
#[derive(Clone)]
pub struct LoginCommand {
    pub principal: Principal,
    /// Credential payload; never log or debug-format it.
    pub upstream_payload: Value,
}

pub struct AuthService {
    sessions: Arc<dyn SessionRepository>,
    authenticator: Arc<dyn Authenticator>,
    /// Sliding session lifetime; reported to clients as `session.expiresInSec` on resume/info.
    session_ttl: Duration,
}

impl AuthService {
    pub async fn create_account(
        &self,
        command: crate::domain::authenticator::CreateAccountCommand,
    ) -> RpcResult<Value> {
        self.authenticator.create_account(command).await
    }

    /// Legacy constructor for explicitly injected HTTP/test gateways. Production uses
    /// [`Self::with_authenticator`] with the MongoDB adapter.
    pub fn new(
        sessions: Arc<dyn SessionRepository>,
        gateway: Arc<dyn DriverApiGateway>,
        session_ttl: Duration,
    ) -> Self {
        Self::with_authenticator(sessions, Arc::new(GatewayAuthenticator(gateway)), session_ttl)
    }

    pub fn with_authenticator(
        sessions: Arc<dyn SessionRepository>,
        authenticator: Arc<dyn Authenticator>,
        session_ttl: Duration,
    ) -> Self {
        Self {
            sessions,
            authenticator,
            session_ttl,
        }
    }

    fn expires_in_sec(&self) -> u64 {
        self.session_ttl.as_secs()
    }

    /// Verifies credentials, registers a resumable session and makes sure the
    /// response carries `session.sessionId`.
    ///
    /// The session id handed to the client is always minted here (CSPRNG). If the upstream
    /// issued its own `session.sessionId`, it is preserved as `session.upstreamSessionId` so the
    /// client can still correlate with the upstream, but it is never used as *our* credential.
    ///
    /// A denied authentication or failed session write must never report a successful login.
    pub async fn login(&self, command: LoginCommand) -> RpcResult<Value> {
        let mut response = self.authenticator.authenticate(command.upstream_payload).await?;
        if response.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(RpcError::invalid_credentials());
        }

        let access_token = extract_access_token(&response).unwrap_or_default();

        let session = self
            .sessions
            .register(&command.principal, &access_token)
            .await
            .map_err(|_| RpcError::session_storage())?;
        attach_session_id(&mut response, &session.id);

        Ok(response)
    }

    pub async fn resume(&self, session_id: &str) -> RpcResult<Value> {
        let session = self.lookup(session_id).await?;
        Ok(json!({
            "status": "ok",
            "resumed": true,
            "session": {
                "sessionId": session.id,
                "tokenType": "Bearer",
                "accessToken": session.access_token,
                "expiresInSec": self.expires_in_sec()
            },
            "user": { "phone": session.principal.phone },
            "device": { "deviceId": session.principal.device_id }
        }))
    }

    /// Like [`Self::resume`] but never discloses the access token.
    pub async fn info(&self, session_id: &str) -> RpcResult<Value> {
        let session = self.lookup(session_id).await?;
        Ok(json!({
            "status": "ok",
            "session": {
                "sessionId": session.id,
                "tokenType": "Bearer",
                "expiresInSec": self.expires_in_sec()
            },
            "user": { "phone": session.principal.phone },
            "device": { "deviceId": session.principal.device_id }
        }))
    }

    /// Operator-only variant that includes the access token. Gated by configuration in the
    /// presentation layer.
    pub async fn debug(&self, session_id: &str) -> RpcResult<Value> {
        let session = self.lookup(session_id).await?;
        Ok(json!({
            "status": "ok",
            "debug": true,
            "session": {
                "sessionId": session.id,
                "tokenType": "Bearer",
                "accessToken": session.access_token,
                "expiresInSec": self.expires_in_sec()
            },
            "user": { "phone": session.principal.phone },
            "device": { "deviceId": session.principal.device_id }
        }))
    }

    pub async fn cleanup_expired(&self) -> anyhow::Result<u64> {
        self.sessions.cleanup_expired().await
    }

    /// Explicit logout: forgets the session so it can no longer be resumed.
    ///
    /// Idempotent — logging out an unknown/expired session is not an error (the client's goal,
    /// "this session must not be usable any more", is already met). The transport layer closes
    /// the QUIC connection after the response has been delivered.
    pub async fn logout(&self, session_id: Option<&str>) -> RpcResult<Value> {
        let removed = match session_id {
            Some(id) => self.sessions.remove(id).await.map_err(|err| {
                warn!("failed to remove session on logout: {err:#}");
                RpcError::session_storage()
            })?,
            None => false,
        };
        debug!("logout session_id={session_id:?} removed={removed}");
        Ok(json!({ "status": "ok", "loggedOut": true, "sessionRemoved": removed }))
    }

    async fn lookup(&self, session_id: &str) -> RpcResult<Session> {
        self.sessions
            .find(session_id)
            .await
            .map_err(|_| RpcError::session_storage())?
            .ok_or_else(RpcError::session_not_found)
    }
}

/// Sets `session.sessionId` to our id. An upstream-issued `sessionId`, if present and different,
/// is moved to `session.upstreamSessionId`.
fn attach_session_id(response: &mut Value, session_id: &str) {
    let Some(map) = response.as_object_mut() else {
        return;
    };
    let session_entry = map
        .entry("session".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !session_entry.is_object() {
        *session_entry = Value::Object(serde_json::Map::new());
    }
    if let Some(session_map) = session_entry.as_object_mut() {
        if let Some(upstream) = session_map
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && *v != session_id)
            .map(str::to_string)
        {
            session_map
                .entry("upstreamSessionId".to_string())
                .or_insert(Value::String(upstream));
        }
        session_map.insert("sessionId".to_string(), Value::String(session_id.to_string()));
    }
}

fn extract_access_token(response: &Value) -> Option<String> {
    let pick = |v: &Value| {
        v.get("accessToken")
            .or_else(|| v.get("access_token"))
            .or_else(|| v.get("token"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    response
        .get("session")
        .and_then(pick)
        .or_else(|| pick(response))
        .filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedAuthenticator(RpcResult<Value>);

    #[async_trait]
    impl Authenticator for FixedAuthenticator {
        async fn authenticate(&self, _: Value) -> RpcResult<Value> {
            self.0.clone()
        }

        async fn create_account(&self, _: crate::domain::authenticator::CreateAccountCommand) -> RpcResult<Value> {
            self.0.clone()
        }
    }

    struct RecordingSessions {
        writes: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl SessionRepository for RecordingSessions {
        async fn register(&self, principal: &Principal, access_token: &str) -> anyhow::Result<Session> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("private database details");
            }
            Ok(Session::new("test-session", principal.clone(), access_token))
        }

        async fn find(&self, _: &str) -> anyhow::Result<Option<Session>> {
            Ok(None)
        }

        async fn remove(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn cleanup_expired(&self) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    fn command() -> LoginCommand {
        LoginCommand {
            principal: Principal::new("driver@example.com", "email-login"),
            upstream_payload: json!({}),
        }
    }

    #[tokio::test]
    async fn create_account_never_registers_a_session() {
        let sessions = Arc::new(RecordingSessions {
            writes: AtomicUsize::new(0),
            fail: true,
        });
        let auth = AuthService::with_authenticator(
            sessions.clone(),
            Arc::new(FixedAuthenticator(Ok(json!({"status": "ok", "created": true})))),
            Duration::from_secs(60),
        );
        let command = crate::domain::authenticator::CreateAccountCommand::new(
            "Test Driver",
            "test@example.com",
            "5f4dcc3b5aa765d61d8327deb882cf99",
        )
        .unwrap();
        let result = auth.create_account(command).await.unwrap();
        assert_eq!(result, json!({"status": "ok", "created": true}));
        assert!(result.get("session").is_none());
        assert_eq!(sessions.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_verified_status_ok_can_register_sessions() {
        let sessions = Arc::new(RecordingSessions {
            writes: AtomicUsize::new(0),
            fail: false,
        });
        for response in [
            Ok(json!({"status": "denied"})),
            Ok(json!({"accessToken": "not-authenticated"})),
            Ok(Value::Null),
            Err(RpcError::invalid_credentials()),
            Err(RpcError::authentication_unavailable()),
        ] {
            let auth = AuthService::with_authenticator(
                sessions.clone(),
                Arc::new(FixedAuthenticator(response)),
                Duration::from_secs(60),
            );
            assert!(auth.login(command()).await.is_err());
        }
        assert_eq!(sessions.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn session_storage_failure_fails_login_without_leaking_details() {
        let sessions = Arc::new(RecordingSessions {
            writes: AtomicUsize::new(0),
            fail: true,
        });
        let auth = AuthService::with_authenticator(
            sessions.clone(),
            Arc::new(FixedAuthenticator(Ok(
                json!({"status": "ok", "accessToken": "local-token"}),
            ))),
            Duration::from_secs(60),
        );
        assert_eq!(auth.login(command()).await.unwrap_err(), RpcError::session_storage());
        assert_eq!(sessions.writes.load(Ordering::SeqCst), 1);
    }
}
