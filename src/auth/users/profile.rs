use std::collections::HashMap;

use json_rpc_2_oauth2::{ProfileContact, validate_contacts};
use mongodb::bson::{Bson, DateTime, Document};
use serde_json::{Map, Value, json};

use super::storage_error;

pub(super) fn user_json(user: Document) -> Result<Value, super::RpcError> {
    let id = user
        .get_object_id("_id")
        .map_err(|_| super::RpcError::internal_error())?;
    let email = user
        .get_str("email")
        .map_err(|_| super::RpcError::internal_error())?;
    let created_at = user
        .get_datetime("created_at")
        .map_err(|_| super::RpcError::internal_error())?;
    let roles = user
        .get_array("roles")
        .map_err(|_| super::RpcError::internal_error())?
        .iter()
        .map(|role| {
            role.as_str()
                .map(str::to_owned)
                .ok_or_else(super::RpcError::internal_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let profile = user.get_document("profile").cloned().unwrap_or_default();
    let mut profile_json = Map::new();
    for (name, value) in profile {
        let value = if name == "contacts" {
            let contacts: Vec<ProfileContact> =
                mongodb::bson::from_bson(value).map_err(storage_error)?;
            json!(contacts)
        } else {
            profile_value(value)?
        };
        profile_json.insert(name, value);
    }
    profile_json
        .entry("contacts")
        .or_insert_with(|| json!([{"type": "email", "value": email}]));
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

fn profile_value(value: Bson) -> Result<Value, super::RpcError> {
    match value {
        Bson::String(value) => Ok(Value::String(value)),
        Bson::Int32(value) => Ok(json!(value)),
        Bson::Int64(value) => Ok(json!(value)),
        Bson::Double(value) if value.is_finite() => Ok(json!(value)),
        Bson::Boolean(value) => Ok(json!(value)),
        Bson::DateTime(value) => Ok(Value::String(
            value.try_to_rfc3339_string().map_err(storage_error)?,
        )),
        _ => Err(super::RpcError::internal_error()),
    }
}

pub(super) fn parse_profile(values: HashMap<String, Value>) -> Result<Document, super::RpcError> {
    let mut profile = Document::new();
    for (name, value) in values {
        if name == "contacts" {
            let value = match value {
                Value::String(raw) => {
                    serde_json::from_str(&raw).map_err(|_| super::RpcError::invalid_params())?
                }
                value => value,
            };
            let mut contacts: Vec<ProfileContact> =
                serde_json::from_value(value).map_err(|_| super::RpcError::invalid_params())?;
            validate_contacts(&mut contacts).map_err(|_| super::RpcError::invalid_params())?;
            profile.insert(
                name,
                mongodb::bson::to_bson(&contacts).map_err(storage_error)?,
            );
            continue;
        }
        let value = value
            .as_str()
            .ok_or_else(super::RpcError::invalid_params)?
            .to_owned();
        let field = match name.as_str() {
            "first_name" | "last_name" | "middle_name" => Bson::String(value),
            "account_type" if matches!(value.as_str(), "private" | "corporate") => {
                Bson::String(value)
            }
            "birth_date" => {
                let date = chrono::DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| super::RpcError::invalid_params())?;
                Bson::DateTime(DateTime::from_millis(date.timestamp_millis()))
            }
            "notifications_enabled" => Bson::Boolean(
                value
                    .parse::<bool>()
                    .map_err(|_| super::RpcError::invalid_params())?,
            ),
            _ => return Err(super::RpcError::invalid_params()),
        };
        profile.insert(name, field);
    }
    Ok(profile)
}
