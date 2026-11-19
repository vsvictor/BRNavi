use std::env;

use json_rpc_2_oauth2::{AuthPlugin, MongoAuthServer, RpcError, RpcServer};
use mongodb::{
    Client, Database,
    bson::doc,
    options::{ClientOptions, Credential},
};
use serde_json::{Value, json};

mod accounts;
mod profile;
mod roles;

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
            let users = accounts::list_users(&database).await?;
            Ok(json!({ "users": users }))
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.list", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            plugin.authorize(params, &["rbac.roles.write"])?;
            let roles = roles::list_assignable_roles(&database).await?;
            Ok(json!({ "roles": roles }))
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.create", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.roles.write"])?;
            let request: roles::CreateRoleRequest = parse_params(request.params)?;
            roles::create_role(&database, request).await
        }
    })?;

    let service = auth.clone();
    let roles_database = database.clone();
    service.register(server, "admin.roles.update", move |plugin, params| {
        let database = roles_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.roles.write"])?;
            let request: roles::UpdateRoleRequest = parse_params(request.params)?;
            roles::update_role(&database, request).await
        }
    })?;

    let service = auth.clone();
    let users_database = database.clone();
    service.register(server, "admin.users.create", move |plugin, params| {
        let database = users_database.clone();
        async move {
            let request = plugin.authorize(params, &["rbac.users.write"])?;
            let request: accounts::CreateUserRequest = parse_params(request.params)?;
            accounts::create_user(&database, request).await
        }
    })?;

    let service = auth.clone();
    let users_database = database.clone();
    service.register(server, "admin.users.update", move |plugin, params| {
        let database = users_database.clone();
        async move {
            let authorized = plugin.authorize(params.clone(), &["rbac.users.write"])?;
            let request: accounts::UpdateUserRequest = parse_params(authorized.params)?;
            if request.roles.is_some() {
                plugin.authorize(params, &["rbac.roles.write"])?;
            }
            accounts::update_user(&database, request).await
        }
    })?;

    let service = auth.clone();
    service.register(server, "admin.users.delete", move |plugin, params| {
        let database = database.clone();
        async move {
            let authorized = plugin.authorize(params, &["rbac.users.write"])?;
            let request: accounts::DeleteUserRequest = parse_params(authorized.params)?;
            if authorized.claims.subject == request.user_id.as_str() {
                return Err(RpcError::invalid_params());
            }
            accounts::delete_user(&database, &request.user_id).await?;
            Ok(Value::Null)
        }
    })?;
    Ok(())
}

pub(super) fn storage_error(_error: impl std::fmt::Display) -> RpcError {
    RpcError::internal_error()
}

pub(super) fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
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

#[cfg(test)]
mod tests;
