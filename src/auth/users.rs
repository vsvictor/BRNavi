use std::{collections::HashMap, env};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use json_rpc_2_oauth2::{AuthPlugin, MongoAuthServer, RpcError, RpcServer};
use mongodb::{
    Client, Collection, Database,
    bson::{Bson, DateTime, Document, doc, oid::ObjectId},
    options::{ClientOptions, Credential},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const USERS: &str = "users";

pub async fn admin_database_from_env() -> anyhow::Result<Database> {
    let host = required_env("AUTH_DB_HOST")?;
    let database_name = required_env("AUTH_DB_NAME")?;
    let mut options = ClientOptions::parse(if host.contains("://") {
        host
    } else {
        format!("mongodb://{host}")
    })
    .await?;

    match (
        optional_env("AUTH_DB_USER"),
        optional_env("AUTH_DB_PASSWORD"),
    ) {
        (Some(username), Some(password)) => {
            let credential = options
                .credential
                .get_or_insert_with(|| Credential::builder().build());
            credential.username = Some(username);
            credential.password = Some(password);
        }
        (None, None) => {}
        _ => anyhow::bail!("AUTH_DB_USER and AUTH_DB_PASSWORD must be configured together"),
    }

    let client = Client::with_options(options)?;
    let database = client.database(&database_name);
    database.run_command(doc! { "ping": 1 }).await?;
    Ok(database)
}

pub fn register_user_admin_methods(
    auth: &AuthPlugin<MongoAuthServer>,
    server: &RpcServer,
    database: Database,
) -> anyhow::Result<()> {
    let service = auth.clone();
    let users_database = database.clone();
    service.register(server, "admin.users.list", move |plugin, params| {
        let database = users_database.clone();
        async move {
            plugin.authorize(params, &["rbac.users.read"])?;
            let users = list_users(&database).await?;
            Ok(json!({ "users": users }))
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.list", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            plugin.authorize(params, &["rbac.roles.write"])?;
            let roles = list_assignable_roles(&database).await?;
            Ok(json!({ "roles": roles }))
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.create", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.roles.write"])?;
            let request: CreateRoleRequest = parse_params(request.params)?;
            create_role(&database, request).await
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.update", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.roles.write"])?;
            let request: UpdateRoleRequest = parse_params(request.params)?;
            update_role(&database, request).await
        }
    })?;

    let service = auth.clone();
    let users_database = database.clone();
    service.register(server, "admin.users.create", move |plugin, params| {
        let database = users_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.users.write"])?;
            let request: CreateUserRequest = parse_params(request.params)?;
            let user = create_user(&database, request).await?;
            Ok(user)
        }
    })?;

    let service = auth.clone();
    let users_database = database.clone();
    service.register(server, "admin.users.update", move |plugin, params| {
        let database = users_database.clone();
        async move {
            let authorized = plugin.authorize(params.clone(), &["rbac.users.write"])?;
            let request: UpdateUserRequest = parse_params(authorized.params)?;
            if request.roles.is_some() {
                plugin.authorize(params, &["rbac.roles.write"])?;
            }
            let user = update_user(&database, request).await?;
            Ok(user)
        }
    })?;

    let service = auth.clone();
    service.register(server, "admin.users.delete", move |plugin, params| {
        let database = database.clone();
        async move {
            let authorized = plugin.authorize(params, &["rbac.users.write"])?;
            let request: DeleteUserRequest = parse_params(authorized.params)?;
            if authorized.claims.subject == request.user_id.as_str() {
                return Err(RpcError::invalid_params());
            }
            delete_user(&database, &request.user_id).await?;
            Ok(Value::Null)
        }
    })?;
    Ok(())
}

async fn list_assignable_roles(database: &Database) -> Result<Vec<Value>, RpcError> {
    let mut cursor = database
        .collection::<Document>("roles")
        .find(doc! {})
        .sort(doc! { "name": 1 })
        .projection(doc! { "name": 1, "permissions": 1, "_id": 0 })
        .await
        .map_err(storage_error)?;
    let mut roles = Vec::new();
    while cursor.advance().await.map_err(storage_error)? {
        let role = cursor.deserialize_current().map_err(storage_error)?;
        let name = role
            .get_str("name")
            .map_err(|_| RpcError::internal_error())?;
        if !name.starts_with("system.") {
            let permissions = role
                .get_array("permissions")
                .map_err(|_| RpcError::internal_error())?
                .iter()
                .map(|permission| {
                    permission
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(RpcError::internal_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            roles.push(json!({ "name": name, "permissions": permissions }));
        }
    }
    Ok(roles)
}

async fn create_role(database: &Database, request: CreateRoleRequest) -> Result<Value, RpcError> {
    let name = parse_role_name(&request.name)?;
    let permissions = parse_role_permissions(request.permissions)?;
    let result = database
        .collection::<Document>("roles")
        .update_one(
            doc! { "name": &name },
            doc! { "$setOnInsert": { "permissions": &permissions } },
        )
        .upsert(true)
        .await
        .map_err(storage_error)?;
    if result.upserted_id.is_none() {
        return Err(RpcError::invalid_params());
    }
    Ok(json!({ "name": name, "permissions": permissions }))
}

async fn update_role(database: &Database, request: UpdateRoleRequest) -> Result<Value, RpcError> {
    let name = parse_role_name(&request.name)?;
    let permissions = parse_role_permissions(request.permissions)?;
    let result = database
        .collection::<Document>("roles")
        .update_one(
            doc! { "name": &name },
            doc! { "$set": { "permissions": &permissions } },
        )
        .await
        .map_err(storage_error)?;
    if result.matched_count == 0 {
        return Err(RpcError::invalid_params());
    }
    Ok(json!({ "name": name, "permissions": permissions }))
}

fn parse_role_name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || name.starts_with("system.")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(RpcError::invalid_params());
    }
    Ok(name.to_owned())
}

fn parse_role_permissions(permissions: Vec<String>) -> Result<Vec<String>, RpcError> {
    let mut validated = Vec::new();
    for permission in permissions {
        let permission = permission.trim();
        if permission.is_empty()
            || permission == "*"
            || permission.len() > 128
            || !permission
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(RpcError::invalid_params());
        }
        if !validated.iter().any(|existing| existing == permission) {
            validated.push(permission.to_owned());
        }
    }
    if validated.is_empty() {
        return Err(RpcError::invalid_params());
    }
    Ok(validated)
}

async fn list_users(database: &Database) -> Result<Vec<Value>, RpcError> {
    let collection = users(database);
    let mut cursor = collection
        .find(doc! {})
        .sort(doc! { "email": 1 })
        .projection(doc! { "password_hash": 0 })
        .await
        .map_err(storage_error)?;
    let mut result = Vec::new();
    while cursor.advance().await.map_err(storage_error)? {
        let user = cursor.deserialize_current().map_err(storage_error)?;
        result.push(user_json(user)?);
    }
    Ok(result)
}

async fn create_user(database: &Database, request: CreateUserRequest) -> Result<Value, RpcError> {
    let email = normalize_email(&request.email)?;
    validate_password(&request.password)?;
    let profile = parse_profile(request.profile.unwrap_or_default())?;
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(request.password.as_bytes(), &salt)
        .map_err(|_| RpcError::internal_error())?
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
    user_json(response)
}

async fn update_user(database: &Database, request: UpdateUserRequest) -> Result<Value, RpcError> {
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
            return Err(RpcError::invalid_params());
        }
        updates.insert("roles", roles);
    }
    if let Some(profile) = request.profile {
        for (name, value) in parse_profile(profile)? {
            updates.insert(format!("profile.{name}"), value);
        }
    }
    if let Some(password) = request.password {
        validate_password(&password)?;
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| RpcError::internal_error())?
            .to_string();
        updates.insert("password_hash", password_hash);
    }
    if updates.is_empty() {
        return Err(RpcError::invalid_params());
    }

    let collection = users(database);
    let updated = collection
        .update_one(doc! { "_id": user_id }, doc! { "$set": updates })
        .await
        .map_err(storage_error)?;
    if updated.matched_count == 0 {
        return Err(RpcError::invalid_params());
    }
    let user = collection
        .find_one(doc! { "_id": user_id })
        .projection(doc! { "password_hash": 0 })
        .await
        .map_err(storage_error)?
        .ok_or_else(RpcError::internal_error)?;
    user_json(user)
}

async fn delete_user(database: &Database, id: &str) -> Result<(), RpcError> {
    let user_id = parse_user_id(id)?;
    let collection = users(database);
    let deleted = collection
        .delete_one(doc! { "_id": user_id })
        .await
        .map_err(storage_error)?;
    if deleted.deleted_count == 0 {
        return Err(RpcError::invalid_params());
    }
    database
        .collection::<Document>("external_identities")
        .delete_many(doc! { "user_id": user_id })
        .await
        .map_err(storage_error)?;
    Ok(())
}

fn user_json(user: Document) -> Result<Value, RpcError> {
    let id = user
        .get_object_id("_id")
        .map_err(|_| RpcError::internal_error())?;
    let email = user
        .get_str("email")
        .map_err(|_| RpcError::internal_error())?;
    let created_at = user
        .get_datetime("created_at")
        .map_err(|_| RpcError::internal_error())?;
    let roles = user
        .get_array("roles")
        .map_err(|_| RpcError::internal_error())?
        .iter()
        .map(|role| {
            role.as_str()
                .map(str::to_owned)
                .ok_or_else(RpcError::internal_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let profile = user.get_document("profile").cloned().unwrap_or_default();
    let mut profile_json = Map::new();
    for (name, value) in profile {
        profile_json.insert(name, profile_value(value)?);
    }
    Ok(json!({
        "id": id.to_hex(),
        "email": email,
        "created_at": created_at
            .try_to_rfc3339_string()
            .map_err(storage_error)?,
        "roles": roles,
        "profile": profile_json,
    }))
}

fn profile_value(value: Bson) -> Result<Value, RpcError> {
    match value {
        Bson::String(value) => Ok(Value::String(value)),
        Bson::Int32(value) => Ok(json!(value)),
        Bson::Int64(value) => Ok(json!(value)),
        Bson::Double(value) if value.is_finite() => Ok(json!(value)),
        Bson::Boolean(value) => Ok(json!(value)),
        Bson::DateTime(value) => Ok(Value::String(
            value.try_to_rfc3339_string().map_err(storage_error)?,
        )),
        _ => Err(RpcError::internal_error()),
    }
}

fn parse_profile(values: HashMap<String, String>) -> Result<Document, RpcError> {
    let mut profile = Document::new();
    for (name, value) in values {
        let field = match name.as_str() {
            "first_name" | "last_name" | "middle_name" => Bson::String(value),
            "birth_date" => {
                let date = chrono::DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| RpcError::invalid_params())?;
                Bson::DateTime(DateTime::from_millis(date.timestamp_millis()))
            }
            "notifications_enabled" => Bson::Boolean(
                value
                    .parse::<bool>()
                    .map_err(|_| RpcError::invalid_params())?,
            ),
            _ => return Err(RpcError::invalid_params()),
        };
        profile.insert(name, field);
    }
    Ok(profile)
}

fn normalize_email(email: &str) -> Result<String, RpcError> {
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
        return Err(RpcError::invalid_params());
    }
    Ok(email)
}

fn validate_password(password: &str) -> Result<(), RpcError> {
    if password.chars().count() < 8 {
        return Err(RpcError::invalid_params());
    }
    Ok(())
}

fn parse_user_id(id: &str) -> Result<ObjectId, RpcError> {
    ObjectId::parse_str(id).map_err(|_| RpcError::invalid_params())
}

fn users(database: &Database) -> Collection<Document> {
    database.collection(USERS)
}

fn storage_error(_error: impl std::fmt::Display) -> RpcError {
    RpcError::internal_error()
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|_| RpcError::invalid_params())
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))?;
    if value.trim().is_empty() {
        anyhow::bail!("{name} must not be empty");
    }
    Ok(value)
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[derive(Deserialize)]
struct CreateUserRequest {
    email: String,
    password: String,
    #[serde(default)]
    profile: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct UpdateUserRequest {
    user_id: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
    #[serde(default)]
    profile: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct DeleteUserRequest {
    user_id: String,
}

#[derive(Deserialize)]
struct CreateRoleRequest {
    name: String,
    permissions: Vec<String>,
}

#[derive(Deserialize)]
struct UpdateRoleRequest {
    name: String,
    permissions: Vec<String>,
}

#[cfg(test)]
mod tests;
