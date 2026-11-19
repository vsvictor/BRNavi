use json_rpc_2_backend::PluginHandle;
use json_rpc_2_car::CarConfig;
use json_rpc_2_oauth2::{
    AuthError, AuthPlugin, JsonRpcAuthServer, RpcError, RpcServer, ServerPlugin,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct AuthorizedCar<S> {
    config: CarConfig,
    auth: AuthPlugin<S>,
}

impl<S> AuthorizedCar<S> {
    pub fn new(config: CarConfig, auth: AuthPlugin<S>) -> Self {
        Self { config, auth }
    }
}

impl<S> ServerPlugin for AuthorizedCar<S>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    const NAME: &'static str = "car";

    async fn install(self, server: &RpcServer) -> anyhow::Result<PluginHandle> {
        let upstream = RpcServer::new();
        let handle = self.config.install(&upstream).await?;
        register_authorized(&self.auth, server, upstream)?;
        Ok(handle)
    }
}

fn register_authorized<S>(
    auth: &AuthPlugin<S>,
    server: &RpcServer,
    upstream: RpcServer,
) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    for method in upstream
        .method_names()
        .into_iter()
        .filter(|name| name.starts_with("car."))
    {
        let auth = auth.clone();
        let upstream = upstream.clone();
        server.register(method.clone(), move |params| {
            let auth = auth.clone();
            let upstream = upstream.clone();
            let method = method.clone();
            async move {
                let request = auth.authorize(params, &[])?;
                dispatch_car(&upstream, &method, request.params).await
            }
        })?;
    }
    register_catalog(auth, server, upstream)?;
    Ok(())
}

fn register_catalog<S>(
    auth: &AuthPlugin<S>,
    server: &RpcServer,
    upstream: RpcServer,
) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    auth.register(server, "catalog.classes.list", |auth, params| async move {
        auth.authorize(params, &[])?;
        Ok(json!({
            "classes": [
                {"id": "passenger", "name": "Легкові"},
                {"id": "truck", "name": "Вантажні"},
            ],
        }))
    })?;

    let makes_upstream = upstream.clone();
    auth.register(server, "catalog.makes.list", move |auth, params| {
        let upstream = makes_upstream.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let request: CatalogMakesRequest = parse(request.params)?;
            if !matches!(request.class_id.as_str(), "passenger" | "truck") {
                return Err(RpcError::invalid_params());
            }
            let makes = catalog_rows(&upstream, "car.makes_index2_get", json!({})).await?;
            Ok(json!({ "makes": makes.iter().map(catalog_make).collect::<Result<Vec<_>, _>>()? }))
        }
    })?;

    auth.register(server, "catalog.models.list", move |auth, params| {
        let upstream = upstream.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let request: CatalogModelsRequest = parse(request.params)?;
            let models = catalog_rows(
                &upstream,
                "car.oemmakemodels_index_get",
                json!({ "make_id": request.make_id }),
            )
            .await?;
            let models = models
                .iter()
                .map(catalog_model)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "models": models }))
        }
    })?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogMakesRequest {
    class_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogModelsRequest {
    make_id: String,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|_| RpcError::invalid_params())
}

async fn catalog_rows(
    upstream: &RpcServer,
    method: &str,
    filters: Value,
) -> Result<Vec<Value>, RpcError> {
    const LIMIT: u64 = 100;
    const MAX_PAGES: u64 = 1000;

    let mut rows = Vec::new();
    let mut page = 1u64;
    loop {
        let mut params = filters.clone();
        let object = params
            .as_object_mut()
            .ok_or_else(RpcError::internal_error)?;
        object.insert("page".into(), json!(page));
        object.insert("limit".into(), json!(LIMIT));
        let response = dispatch_car(upstream, method, params).await?;
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(RpcError::internal_error)?;
        rows.extend(data.iter().cloned());

        let pages = match response.get("collection") {
            None => 1,
            Some(collection) => collection
                .get("pages")
                .and_then(Value::as_u64)
                .filter(|pages| *pages <= MAX_PAGES)
                .ok_or_else(RpcError::internal_error)?,
        };
        if pages == 0 || page >= pages {
            return Ok(rows);
        }
        page += 1;
    }
}

fn catalog_make(item: &Value) -> Result<Value, RpcError> {
    Ok(json!({
        "id": catalog_id(item.get("id").ok_or_else(RpcError::internal_error)?)?,
        "name": catalog_name(item)?,
    }))
}

fn catalog_model(item: &Value) -> Result<Value, RpcError> {
    Ok(json!({
        "id": catalog_id(item.get("id").ok_or_else(RpcError::internal_error)?)?,
        "name": catalog_name(item)?,
        "years": [],
        "trims": [],
    }))
}

fn catalog_id(value: &Value) -> Result<String, RpcError> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|id| id.to_string()))
        .or_else(|| value.as_u64().map(|id| id.to_string()))
        .ok_or_else(RpcError::internal_error)
}

fn catalog_name(item: &Value) -> Result<&str, RpcError> {
    item.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(RpcError::internal_error)
}

async fn dispatch_car(
    upstream: &RpcServer,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let input = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .map_err(dispatch_error)?;
    let output = upstream.handle_bytes(&input).await.ok_or_else(|| {
        eprintln!("Car RPC dispatcher returned no response");
        RpcError::internal_error()
    })?;
    let response: Value = serde_json::from_slice(&output).map_err(dispatch_error)?;
    if let Some(error) = response.get("error") {
        return Err(serde_json::from_value(error.clone()).map_err(dispatch_error)?);
    }
    response.get("result").cloned().ok_or_else(|| {
        eprintln!("Car RPC dispatcher returned neither result nor error");
        RpcError::internal_error()
    })
}

fn dispatch_error(error: serde_json::Error) -> RpcError {
    eprintln!("Car RPC dispatch serialization failed: {error}");
    RpcError::internal_error()
}

#[cfg(test)]
mod tests;
