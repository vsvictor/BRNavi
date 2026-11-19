use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::storage_error;

pub(super) async fn list_assignable_roles(
    database: &Database,
) -> Result<Vec<Value>, super::RpcError> {
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
            .map_err(|_| super::RpcError::internal_error())?;
        if !name.starts_with("system.") {
            let permissions = role
                .get_array("permissions")
                .map_err(|_| super::RpcError::internal_error())?
                .iter()
                .map(|permission| {
                    permission
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(super::RpcError::internal_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            roles.push(json!({ "name": name, "permissions": permissions }));
        }
    }
    Ok(roles)
}

pub(super) async fn create_role(
    database: &Database,
    request: CreateRoleRequest,
) -> Result<Value, super::RpcError> {
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
        return Err(super::RpcError::invalid_params());
    }
    Ok(json!({ "name": name, "permissions": permissions }))
}

pub(super) async fn update_role(
    database: &Database,
    request: UpdateRoleRequest,
) -> Result<Value, super::RpcError> {
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
        return Err(super::RpcError::invalid_params());
    }
    Ok(json!({ "name": name, "permissions": permissions }))
}

pub(super) fn parse_role_name(name: &str) -> Result<String, super::RpcError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || name.starts_with("system.")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(super::RpcError::invalid_params());
    }
    Ok(name.to_owned())
}

pub(super) fn parse_role_permissions(
    permissions: Vec<String>,
) -> Result<Vec<String>, super::RpcError> {
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
            return Err(super::RpcError::invalid_params());
        }
        if !validated.iter().any(|existing| existing == permission) {
            validated.push(permission.to_owned());
        }
    }
    if validated.is_empty() {
        return Err(super::RpcError::invalid_params());
    }
    Ok(validated)
}

#[derive(Deserialize)]
pub(super) struct CreateRoleRequest {
    name: String,
    permissions: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct UpdateRoleRequest {
    name: String,
    permissions: Vec<String>,
}
