//! Port: session persistence.

use anyhow::Result;
use async_trait::async_trait;

use super::session::{Principal, Session};

#[async_trait]
pub trait SessionRepository: Send + Sync {
    /// Creates a session for `principal` or refreshes the active one.
    ///
    /// When an active session already exists its TTL is extended and the access token replaced;
    /// otherwise a new session is created with a freshly generated, unguessable id. The id is a
    /// bearer credential (`rpc.resume` hands out the access token), so it is never derived from
    /// client- or upstream-supplied data.
    async fn register(&self, principal: &Principal, access_token: &str) -> Result<Session>;

    /// Looks a session up by id, extending its TTL on hit. Expired sessions are treated as absent.
    async fn find(&self, session_id: &str) -> Result<Option<Session>>;

    /// Deletes a session (explicit logout). Returns `true` when a session was actually removed.
    async fn remove(&self, session_id: &str) -> Result<bool>;

    /// Removes expired sessions; returns how many were removed.
    async fn cleanup_expired(&self) -> Result<u64>;
}
