//! MongoDB adapters. They share the OAuth database so the `sessions`
//! collection written by `json-rpc-2-oauth2` is read, never written, here.

use async_trait::async_trait;
use json_rpc_2_coopnav::{
    AppendResult, CoopNavStorage, StorageError, StoragePartition, TrackPoint,
};
use mongodb::{
    Collection, Database, IndexModel,
    bson::{self, DateTime, Document, doc, oid::ObjectId},
    options::IndexOptions,
};

use super::{
    DeviceSession, DeviceSessions, StoreError, TRACKING_PERMISSIONS, TRACKING_ROLE, TrackingGrant,
    TrackingGrants, TrackingPermissions,
};

/// Uses the OAuth `roles.permissions` / `users.roles` schema, just like the
/// host's admin role provisioning, without replacing any existing user roles.
pub struct MongoTrackingPermissions {
    roles: Collection<Document>,
    users: Collection<Document>,
}

impl MongoTrackingPermissions {
    pub fn new(database: &Database) -> Self {
        Self {
            roles: database.collection("roles"),
            users: database.collection("users"),
        }
    }

    pub async fn initialize(&self) -> anyhow::Result<()> {
        self.roles
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "name": 1 })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
            )
            .await?;
        match self
            .roles
            .update_one(
                doc! { "name": TRACKING_ROLE },
                doc! { "$setOnInsert": { "permissions": TRACKING_PERMISSIONS.to_vec() } },
            )
            .upsert(true)
            .await
        {
            Ok(_) => {}
            Err(error) if duplicate_key(&error) => {}
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            self.role_is_safe()
                .await
                .map_err(|_| { anyhow::anyhow!("Cannot verify the host tracking role") })?,
            "The host tracking role must contain exactly the two tracking permissions"
        );
        Ok(())
    }

    async fn role_is_safe(&self) -> Result<bool, StoreError> {
        Ok(self
            .roles
            .find_one(doc! { "name": TRACKING_ROLE })
            .await
            .map_err(|_| StoreError)?
            .is_some_and(|role| tracking_role_is_safe(&role)))
    }
}

pub(super) fn tracking_role_is_safe(role: &Document) -> bool {
    role.get_str("name") == Ok(TRACKING_ROLE)
        && role.get_array("permissions").is_ok_and(|permissions| {
            permissions.len() == TRACKING_PERMISSIONS.len()
                && TRACKING_PERMISSIONS.iter().all(|expected| {
                    permissions
                        .iter()
                        .any(|actual| actual.as_str() == Some(expected))
                })
        })
}

#[async_trait]
impl TrackingPermissions for MongoTrackingPermissions {
    async fn ensure_owner(&self, subject: &str) -> Result<(), StoreError> {
        if !self.role_is_safe().await? {
            return Err(StoreError);
        }
        let user_id = ObjectId::parse_str(subject).map_err(|_| StoreError)?;
        let result = self
            .users
            .update_one(
                doc! { "_id": user_id },
                doc! { "$addToSet": { "roles": TRACKING_ROLE } },
            )
            .await
            .map_err(|_| StoreError)?;
        if result.matched_count != 1 {
            return Err(StoreError);
        }
        Ok(())
    }
}

/// Read-only view of the OAuth `sessions` collection
/// (`{user_id, device_id, sid, last_seen, ...}`, unique on `(user_id, device_id)`).
#[derive(Clone)]
pub struct MongoDeviceSessions {
    collection: Collection<Document>,
}

impl MongoDeviceSessions {
    pub fn new(database: &Database) -> Self {
        Self {
            collection: database.collection("sessions"),
        }
    }
}

#[async_trait]
impl DeviceSessions for MongoDeviceSessions {
    async fn current(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<DeviceSession>, StoreError> {
        let Some(document) = self
            .collection
            .find_one(doc! { "user_id": user_id, "device_id": device_id })
            .projection(doc! { "_id": 0, "sid": 1, "last_seen": 1 })
            .await
            .map_err(|_| StoreError)?
        else {
            return Ok(None);
        };
        let sid = document.get_str("sid").map_err(|_| StoreError)?;
        let last_seen = document.get_datetime("last_seen").map_err(|_| StoreError)?;
        Ok(Some(DeviceSession {
            sid: sid.to_owned(),
            last_seen_ms: last_seen.timestamp_millis(),
        }))
    }
}

/// `tracking_sessions`: `_id = {subject, device_id, auth_sid}` makes one grant
/// per OAuth session atomic; `session_id` has its own unique index.
#[derive(Clone)]
pub struct MongoTrackingGrants {
    collection: Collection<Document>,
}

impl MongoTrackingGrants {
    pub fn new(database: &Database) -> Self {
        Self {
            collection: database.collection("tracking_sessions"),
        }
    }

    pub async fn ensure_indexes(&self) -> mongodb::error::Result<()> {
        self.collection
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "session_id": 1 })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
            )
            .await?;
        Ok(())
    }
}

pub(super) fn grant_key(subject: &str, device_id: &str, auth_sid: &str) -> Document {
    doc! { "subject": subject, "device_id": device_id, "auth_sid": auth_sid }
}

pub(super) fn grant_from_document(document: &Document) -> Option<TrackingGrant> {
    let key = document.get_document("_id").ok()?;
    Some(TrackingGrant {
        session_id: document.get_str("session_id").ok()?.to_owned(),
        subject: key.get_str("subject").ok()?.to_owned(),
        device_id: key.get_str("device_id").ok()?.to_owned(),
        auth_sid: key.get_str("auth_sid").ok()?.to_owned(),
    })
}

#[async_trait]
impl TrackingGrants for MongoTrackingGrants {
    async fn open(&self, candidate: TrackingGrant) -> Result<TrackingGrant, StoreError> {
        let key = grant_key(
            &candidate.subject,
            &candidate.device_id,
            &candidate.auth_sid,
        );
        let update = doc! { "$setOnInsert": {
            "session_id": &candidate.session_id,
            "created_at": DateTime::now(),
        } };
        match self
            .collection
            .update_one(doc! { "_id": key.clone() }, update)
            .upsert(true)
            .await
        {
            Ok(_) => {}
            // A concurrent open for the same OAuth session won the upsert.
            Err(error) if duplicate_key(&error) => {}
            Err(_) => return Err(StoreError),
        }
        let stored = self
            .collection
            .find_one(doc! { "_id": key })
            .await
            .map_err(|_| StoreError)?
            .and_then(|document| grant_from_document(&document))
            .ok_or(StoreError)?;
        // Grants of replaced OAuth sessions of this device are dead anyway.
        let _ = self
            .collection
            .delete_many(doc! {
                "_id.subject": &stored.subject,
                "_id.device_id": &stored.device_id,
                "_id.auth_sid": { "$ne": &stored.auth_sid },
            })
            .await;
        Ok(stored)
    }

    async fn find(&self, session_id: &str) -> Result<Option<TrackingGrant>, StoreError> {
        Ok(self
            .collection
            .find_one(doc! { "session_id": session_id })
            .await
            .map_err(|_| StoreError)?
            .and_then(|document| grant_from_document(&document)))
    }
}

/// `tracker_points`: `_id = {brand_id, driver_id, point_id}` gives atomic,
/// per-owner idempotency without Redis.
#[derive(Clone)]
pub struct MongoCoopNavStorage {
    collection: Collection<Document>,
}

impl MongoCoopNavStorage {
    pub fn new(database: &Database) -> Self {
        Self {
            collection: database.collection("tracker_points"),
        }
    }
}

pub(super) fn point_key(partition: &StoragePartition, point_id: &str) -> Document {
    doc! {
        "brand_id": &partition.brand_id,
        "driver_id": &partition.driver_id,
        "point_id": point_id,
    }
}

pub(super) fn point_document(
    partition: &StoragePartition,
    point: &TrackPoint,
) -> Result<Document, StorageError> {
    let encoded =
        bson::to_document(point).map_err(|error| StorageError::Serialization(error.to_string()))?;
    Ok(doc! {
        "_id": point_key(partition, &point.id),
        "point": encoded,
        "received_at": DateTime::now(),
    })
}

pub(super) fn stored_point(document: &Document) -> Result<TrackPoint, StorageError> {
    let point = document
        .get_document("point")
        .map_err(|error| StorageError::Serialization(error.to_string()))?;
    bson::from_document(point.clone())
        .map_err(|error| StorageError::Serialization(error.to_string()))
}

#[async_trait]
impl CoopNavStorage for MongoCoopNavStorage {
    async fn append_point(
        &self,
        partition: &StoragePartition,
        point: &TrackPoint,
    ) -> Result<AppendResult, StorageError> {
        let document = point_document(partition, point)?;
        match self.collection.insert_one(document).await {
            Ok(_) => Ok(AppendResult::Inserted),
            Err(error) if duplicate_key(&error) => {
                let existing = self
                    .collection
                    .find_one(doc! { "_id": point_key(partition, &point.id) })
                    .await
                    .map_err(|_| StorageError::UnexpectedResponse)?
                    .ok_or(StorageError::UnexpectedResponse)?;
                if stored_point(&existing)? == *point {
                    Ok(AppendResult::AlreadyPresent)
                } else {
                    Ok(AppendResult::Conflict)
                }
            }
            Err(_) => Err(StorageError::UnexpectedResponse),
        }
    }
}

fn duplicate_key(error: &mongodb::error::Error) -> bool {
    matches!(
        error.kind.as_ref(),
        mongodb::error::ErrorKind::Write(mongodb::error::WriteFailure::WriteError(error))
            if error.code == 11000
    )
}
