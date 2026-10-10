//! In-process session store (single instance, lost on restart). Used for tests and dev setups.

use std::{
    collections::HashMap,
    sync::RwLock,
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{Principal, Session, SessionRepository};

use super::generate_session_id;

pub struct InMemorySessionRepository {
    ttl: Duration,
    by_principal: RwLock<HashMap<String, String>>,
    by_session_id: RwLock<HashMap<String, (Session, Instant)>>,
}

impl InMemorySessionRepository {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            by_principal: RwLock::new(HashMap::new()),
            by_session_id: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SessionRepository for InMemorySessionRepository {
    async fn register(&self, principal: &Principal, access_token: &str) -> Result<Session> {
        let principal_key = principal.key();
        let now = Instant::now();

        let existing = self
            .by_principal
            .read()
            .ok()
            .and_then(|map| map.get(&principal_key).cloned());

        if let Some(existing_id) = existing {
            if let Ok(mut sessions) = self.by_session_id.write() {
                if let Some((session, expires_at)) = sessions.get_mut(&existing_id)
                    && *expires_at > now
                {
                    *expires_at = now + self.ttl;
                    session.access_token = access_token.to_string();
                    return Ok(session.clone());
                }
                sessions.remove(&existing_id);
            }
            if let Ok(mut principals) = self.by_principal.write() {
                principals.remove(&principal_key);
            }
        }

        let session_id = generate_session_id();
        let session = Session::new(session_id.clone(), principal.clone(), access_token);

        if let Ok(mut map) = self.by_principal.write() {
            map.insert(principal_key, session_id.clone());
        }
        if let Ok(mut map) = self.by_session_id.write() {
            map.insert(session_id, (session.clone(), now + self.ttl));
        }

        Ok(session)
    }

    async fn find(&self, session_id: &str) -> Result<Option<Session>> {
        let now = Instant::now();
        let Ok(mut sessions) = self.by_session_id.write() else {
            return Ok(None);
        };
        let Some((session, expires_at)) = sessions.get_mut(session_id) else {
            return Ok(None);
        };
        if *expires_at <= now {
            let principal_key = session.principal.key();
            sessions.remove(session_id);
            if let Ok(mut principals) = self.by_principal.write() {
                principals.remove(&principal_key);
            }
            return Ok(None);
        }
        *expires_at = now + self.ttl;
        Ok(Some(session.clone()))
    }

    async fn remove(&self, session_id: &str) -> Result<bool> {
        let Ok(mut sessions) = self.by_session_id.write() else {
            return Ok(false);
        };
        let Some((session, _)) = sessions.remove(session_id) else {
            return Ok(false);
        };
        if let Ok(mut principals) = self.by_principal.write() {
            let key = session.principal.key();
            if principals.get(&key).map(String::as_str) == Some(session_id) {
                principals.remove(&key);
            }
        }
        Ok(true)
    }

    async fn cleanup_expired(&self) -> Result<u64> {
        let now = Instant::now();
        let mut removed = 0u64;

        if let (Ok(mut sessions), Ok(mut principals)) = (self.by_session_id.write(), self.by_principal.write()) {
            let expired_ids: Vec<String> = sessions
                .iter()
                .filter(|(_, (_, expires_at))| *expires_at <= now)
                .map(|(id, _)| id.clone())
                .collect();

            for session_id in expired_ids {
                if let Some((session, _)) = sessions.remove(&session_id) {
                    principals.remove(&session.principal.key());
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }
}
