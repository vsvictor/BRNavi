use anyhow::{Context, ensure};
use json_rpc_2_oauth2::ProfileFieldType;
use json_rpc_2_oauth2::{
    AuthError, AuthPlugin, AuthorizedRequest, JsonRpcAuthServer, MongoAuthServer, RpcError,
    RpcServer, TokenConfig, authorize,
};
use serde_json::{Value, json};
use std::collections::HashMap;

mod users;

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).with_context(|| format!("Missing required variable {name}"))?;
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}

pub fn plugin_from_env() -> anyhow::Result<AuthPlugin<MongoAuthServer>> {
    let mut storage = MongoAuthServer::new(
        &required_env("PASETO_LOCAL_KEY")?,
        TokenConfig::new(required_env("AUTH_ISSUER")?),
    )
    .context("Invalid authentication configuration")?;
    storage.set_db_host(&required_env("AUTH_DB_HOST")?);
    storage.set_db_name(&required_env("AUTH_DB_NAME")?);
    storage.set_db_user(&required_env("AUTH_DB_USER")?);
    storage.set_db_password(&required_env("AUTH_DB_PASSWORD")?);
    storage.set_profile_schema(HashMap::from([
        ("first_name".to_owned(), ProfileFieldType::Text),
        ("last_name".to_owned(), ProfileFieldType::Text),
        ("middle_name".to_owned(), ProfileFieldType::Text),
        ("birth_date".to_owned(), ProfileFieldType::DateTime),
        (
            "notifications_enabled".to_owned(),
            ProfileFieldType::Boolean,
        ),
    ]))?;
    Ok(AuthPlugin::new(storage))
}

pub fn register<S>(auth: &AuthPlugin<S>, rpc: &RpcServer) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    auth.register(rpc, "navi.me", me)?;
    Ok(())
}

pub use users::{admin_database_from_env, register_user_admin_methods};

#[authorize]
async fn me<S>(auth: AuthPlugin<S>, request: AuthorizedRequest) -> Result<Value, RpcError>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    Ok(json!({
        "user_id": request.claims.subject,
        "roles": request.claims.roles,
        "permissions": request.claims.permissions,
    }))
}

#[cfg(test)]
mod tests;
