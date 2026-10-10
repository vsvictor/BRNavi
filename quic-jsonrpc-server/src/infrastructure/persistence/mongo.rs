//! MongoDB-backed session store shared by all server instances.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use mongodb::{
    Client, Collection, IndexModel,
    bson::{DateTime, doc},
    options::IndexOptions,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::{Principal, Session, SessionRepository},
    infrastructure::config::MongoOptions,
};

use super::generate_session_id;

const SESSIONS_COLLECTION: &str = "sessions";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionDocument {
    #[serde(rename = "_id")]
    session_id: String,
    principal_key: String,
    phone: String,
    device_id: String,
    access_token: String,
    expires_at: DateTime,
    created_at: DateTime,
    updated_at: DateTime,
}

impl From<SessionDocument> for Session {
    fn from(doc: SessionDocument) -> Self {
        Session::new(
            doc.session_id,
            Principal::new(doc.phone, doc.device_id),
            doc.access_token,
        )
    }
}

pub struct MongoSessionRepository {
    ttl: Duration,
    collection: Collection<SessionDocument>,
}

impl MongoSessionRepository {
    pub async fn connect(options: &MongoOptions, ttl: Duration) -> Result<Self> {
        let client = Client::with_uri_str(&options.uri)
            .await
            .context("failed to connect to MongoDB")?;
        let collection = client
            .database(&options.database)
            .collection::<SessionDocument>(SESSIONS_COLLECTION);

        let principal_index = IndexModel::builder()
            .keys(doc! {"principal_key": 1})
            .options(IndexOptions::builder().unique(Some(true)).build())
            .build();
        let expires_index = IndexModel::builder()
            .keys(doc! {"expires_at": 1})
            .options(
                IndexOptions::builder()
                    .expire_after(Some(Duration::from_secs(0)))
                    .build(),
            )
            .build();

        collection
            .create_index(principal_index)
            .await
            .context("failed to create principal_key index")?;
        collection
            .create_index(expires_index)
            .await
            .context("failed to create expires_at TTL index")?;

        Ok(Self { ttl, collection })
    }

    fn expires_at(&self) -> DateTime {
        DateTime::from_system_time(std::time::SystemTime::now() + self.ttl)
    }
}

/// MongoDB error code 11000 — unique index violation.
fn is_duplicate_key(err: &mongodb::error::Error) -> bool {
    use mongodb::error::{ErrorKind, WriteFailure};
    match &*err.kind {
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.code == 11000,
        ErrorKind::BulkWrite(bulk) => bulk.write_errors.values().any(|e| e.code == 11000),
        _ => false,
    }
}

#[async_trait]
impl SessionRepository for MongoSessionRepository {
    async fn register(&self, principal: &Principal, access_token: &str) -> Result<Session> {
        let principal_key = principal.key();
        let now = DateTime::now();
        let expires_at = self.expires_at();

        if let Some(existing) = self
            .collection
            .find_one(doc! {"principal_key": &principal_key, "expires_at": {"$gt": now}})
            .await
            .context("failed to query active session")?
        {
            self.collection
                .update_one(
                    doc! {"_id": &existing.session_id},
                    doc! {"$set": {
                        "access_token": access_token,
                        "expires_at": &expires_at,
                        "updated_at": DateTime::now(),
                    }},
                )
                .await
                .context("failed to extend active session")?;
            return Ok(Session::new(existing.session_id, principal.clone(), access_token));
        }

        // An expired document for this principal may still be around (the TTL monitor runs
        // roughly once a minute). It has a different `_id`, and `_id` is immutable, so a
        // `replace_one` would be rejected — delete it explicitly before inserting the new one.
        self.collection
            .delete_many(doc! {"principal_key": &principal_key})
            .await
            .context("failed to remove stale session")?;

        let session_id = generate_session_id();
        let payload = SessionDocument {
            session_id: session_id.clone(),
            principal_key: principal_key.clone(),
            phone: principal.phone.clone(),
            device_id: principal.device_id.clone(),
            access_token: access_token.to_string(),
            expires_at,
            created_at: DateTime::now(),
            updated_at: DateTime::now(),
        };

        match self.collection.insert_one(payload).await {
            Ok(_) => Ok(Session::new(session_id, principal.clone(), access_token)),
            // Two concurrent logins of the same principal raced on the unique `principal_key`
            // index: the other one won, so hand out *its* session instead of failing.
            Err(err) if is_duplicate_key(&err) => {
                let existing = self
                    .collection
                    .find_one(doc! {"principal_key": &principal_key})
                    .await
                    .context("failed to re-read session after duplicate key")?
                    .context("session vanished after duplicate key error")?;
                self.collection
                    .update_one(
                        doc! {"_id": &existing.session_id},
                        doc! {"$set": {
                            "access_token": access_token,
                            "expires_at": self.expires_at(),
                            "updated_at": DateTime::now(),
                        }},
                    )
                    .await
                    .context("failed to refresh session after duplicate key")?;
                Ok(Session::new(existing.session_id, principal.clone(), access_token))
            }
            Err(err) => Err(anyhow::Error::new(err).context("failed to insert session")),
        }
    }

    async fn find(&self, session_id: &str) -> Result<Option<Session>> {
        let now = DateTime::now();
        let found = self
            .collection
            .find_one(doc! {"_id": session_id, "expires_at": {"$gt": now}})
            .await
            .context("failed to fetch session")?;

        let Some(doc) = found else {
            return Ok(None);
        };

        self.collection
            .update_one(
                doc! {"_id": &doc.session_id},
                doc! {"$set": {"expires_at": self.expires_at(), "updated_at": DateTime::now()}},
            )
            .await
            .context("failed to extend resumed session")?;

        Ok(Some(doc.into()))
    }

    async fn remove(&self, session_id: &str) -> Result<bool> {
        let result = self
            .collection
            .delete_one(doc! {"_id": session_id})
            .await
            .context("failed to delete session")?;
        Ok(result.deleted_count > 0)
    }

    async fn cleanup_expired(&self) -> Result<u64> {
        let result = self
            .collection
            .delete_many(doc! {"expires_at": {"$lte": DateTime::now()}})
            .await
            .context("failed to cleanup expired sessions")?;
        Ok(result.deleted_count)
    }
}
