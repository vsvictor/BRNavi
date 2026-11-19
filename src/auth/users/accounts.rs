use std::collections::HashMap;

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use mongodb::{
    Collection, Database,
    bson::{DateTime, Document, doc, oid::ObjectId},
};
use serde::Deserialize;
use serde_json::Value;

use super::{profile, storage_error};

const USERS: &str = "users";

pub(super) async fn list_users(database: &Database) -> Result<Vec<Value>, super::RpcError> {
    let mut cursor = users(database)
        .find(doc! {})
        .sort(doc! { "email": 1 })
        .projection(doc! { "password_hash": 0 })
        .await
        .map_err(storage_error)?;
    let mut result = Vec::new();
    while cursor.advance().await.map_err(storage_error)? {
        let user = cursor.deserialize_current().map_err(storage_error)?;
        result.push(profile::user_json(user)?);
    }
    Ok(result)
}

pub(super) async fn create_user(
    database: &Database,
    request: CreateUserRequest,
) -> Result<Value, super::RpcError> {
    let email = normalize_email(&request.email)?;
    validate_password(&request.password)?;
    let profile = profile::parse_profile(request.profile.unwrap_or_default())?;
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(request.password.as_bytes(), &salt)
        .map_err(|_| super::RpcError::internal_error())?
        .to_string();
    let user = doc! {
        "email": email,
        "password_hash": password_hash,
        "created_at": DateTime::now(),
        "roles": Vec::<String>::new(),
        "profile": profile,
    };
    let mut response = user.clone();
    let result = users(database)
        .insert_one(user)
        .await
        .map_err(storage_error)?;
    response.insert("_id", result.inserted_id);
    profile::user_json(response)
}

pub(super) async fn update_user(
    database: &Database,
    request: UpdateUserRequest,
) -> Result<Value, super::RpcError> {
    let user_id = parse_user_id(&request.user_id)?;
    let mut updates = Document::new();

    if let Some(email) = request.email {
        updates.insert("email", normalize_email(&email)?);
    }
    if let Some(roles) = request.roles {
        if roles
            .iter()
            .any(|role| role.trim().is_empty() || role.len() > 128 || role.starts_with("system."))
        {
            return Err(super::RpcError::invalid_params());
        }
        updates.insert("roles", roles);
    }
    if let Some(profile) = request.profile {
        for (name, value) in profile::parse_profile(profile)? {
            updates.insert(format!("profile.{name}"), value);
        }
    }
    if let Some(password) = request.password {
        validate_password(&password)?;
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| super::RpcError::internal_error())?
            .to_string();
        updates.insert("password_hash", password_hash);
    }
    if updates.is_empty() {
        return Err(super::RpcError::invalid_params());
    }

    let collection = users(database);
    let updated = collection
        .update_one(doc! { "_id": user_id }, doc! { "$set": updates })
        .await
        .map_err(storage_error)?;
    if updated.matched_count == 0 {
        return Err(super::RpcError::invalid_params());
    }
    let user = collection
        .find_one(doc! { "_id": user_id })
        .projection(doc! { "password_hash": 0 })
        .await
        .map_err(storage_error)?
        .ok_or_else(super::RpcError::internal_error)?;
    profile::user_json(user)
}

pub(super) async fn delete_user(database: &Database, id: &str) -> Result<(), super::RpcError> {
    let user_id = parse_user_id(id)?;
    let collection = users(database);
    let deleted = collection
        .delete_one(doc! { "_id": user_id })
        .await
        .map_err(storage_error)?;
    if deleted.deleted_count == 0 {
        return Err(super::RpcError::invalid_params());
    }
    database
        .collection::<Document>("external_identities")
        .delete_many(doc! { "user_id": user_id })
        .await
        .map_err(storage_error)?;
    Ok(())
}

fn normalize_email(email: &str) -> Result<String, super::RpcError> {
    let email = email.trim().to_ascii_lowercase();
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if email.len() > 254
        || local.is_empty()
        || domain.is_empty()
        || parts.next().is_some()
        || !domain.contains('.')
        || email.chars().any(char::is_whitespace)
    {
        return Err(super::RpcError::invalid_params());
    }
    Ok(email)
}

fn validate_password(password: &str) -> Result<(), super::RpcError> {
    if password.chars().count() < 8 {
        return Err(super::RpcError::invalid_params());
    }
    Ok(())
}

fn parse_user_id(id: &str) -> Result<ObjectId, super::RpcError> {
    ObjectId::parse_str(id).map_err(|_| super::RpcError::invalid_params())
}

fn users(database: &Database) -> Collection<Document> {
    database.collection(USERS)
}

#[derive(Deserialize)]
pub(super) struct CreateUserRequest {
    pub(super) email: String,
    password: String,
    #[serde(default)]
    pub(super) profile: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(super) struct UpdateUserRequest {
    user_id: String,
    email: Option<String>,
    password: Option<String>,
    pub(super) roles: Option<Vec<String>>,
    profile: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(super) struct DeleteUserRequest {
    pub(super) user_id: String,
}
