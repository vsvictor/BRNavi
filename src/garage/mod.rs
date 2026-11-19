use std::future::Future;

use json_rpc_2_oauth2::{AuthError, AuthPlugin, JsonRpcAuthServer, RpcError, RpcServer};
use mongodb::bson::{Bson, DateTime, Document, doc, oid::ObjectId};
use serde::Deserialize;
use serde_json::{Value, json};

mod storage;
pub use storage::MongoGarage;

#[derive(Default)]
struct GarageState {
    vehicles: Vec<Document>,
    active_vehicle_id: Option<String>,
}

trait GarageStore: Clone + Send + Sync + 'static {
    fn add(
        &self,
        owner: String,
        vehicle: Document,
    ) -> impl Future<Output = Result<(), RpcError>> + Send;
    fn list(&self, owner: String) -> impl Future<Output = Result<GarageState, RpcError>> + Send;
    fn set_active(
        &self,
        owner: String,
        vehicle_id: String,
    ) -> impl Future<Output = Result<bool, RpcError>> + Send;
    fn update(
        &self,
        owner: String,
        vehicle_id: String,
        vehicle: Document,
    ) -> impl Future<Output = Result<Option<Document>, RpcError>> + Send;
    fn delete(
        &self,
        owner: String,
        vehicle_id: String,
    ) -> impl Future<Output = Result<Option<Option<String>>, RpcError>> + Send;
}

pub fn register<S>(auth: &AuthPlugin<S>, rpc: &RpcServer, store: MongoGarage) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    register_with_store(auth, rpc, store)
}

fn register_with_store<S, T>(auth: &AuthPlugin<S>, rpc: &RpcServer, store: T) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
    T: GarageStore,
{
    let add_store = store.clone();
    auth.register(rpc, "garage.vehicles.add", move |auth, params| {
        let store = add_store.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let input: StrictAddRequest = parse(request.params)?;
            let record = input.input.record()?;
            let result = public_record(&record)?;
            store.add(request.claims.subject, record).await?;
            Ok(json!({ "vehicle": result }))
        }
    })?;
    let list_store = store.clone();
    auth.register(rpc, "garage.vehicles.list", move |auth, params| {
        let store = list_store.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let _: ListRequest = parse(request.params)?;
            let state = store.list(request.claims.subject).await?;
            let vehicles = state
                .vehicles
                .iter()
                .map(public_record)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({
                "vehicles": vehicles,
                "active_vehicle_id": state.active_vehicle_id,
            }))
        }
    })?;
    let active_store = store.clone();
    auth.register(rpc, "garage.vehicles.set_active", move |auth, params| {
        let store = active_store.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let input: ActiveRequest = parse(request.params)?;
            nonempty(&input.vehicle_id)?;
            if !store
                .set_active(request.claims.subject, input.vehicle_id.clone())
                .await?
            {
                // Missing and foreign-owned IDs deliberately have the same response.
                return Err(RpcError::invalid_params());
            }
            Ok(json!({ "active_vehicle_id": input.vehicle_id }))
        }
    })?;
    let update_store = store.clone();
    auth.register(rpc, "garage.vehicles.update", move |auth, params| {
        let store = update_store.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let input: UpdateRequest = parse(request.params)?;
            nonempty(&input.vehicle_id)?;
            let record = input.input.record()?;
            let updated = store
                .update(request.claims.subject, input.vehicle_id, record)
                .await?
                .ok_or_else(RpcError::invalid_params)?;
            Ok(json!({ "vehicle": public_record(&updated)? }))
        }
    })?;
    auth.register(rpc, "garage.vehicles.delete", move |auth, params| {
        let store = store.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let input: ActiveRequest = parse(request.params)?;
            nonempty(&input.vehicle_id)?;
            let active = store
                .delete(request.claims.subject, input.vehicle_id)
                .await?
                .ok_or_else(RpcError::invalid_params)?;
            Ok(json!({ "active_vehicle_id": active }))
        }
    })?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveRequest {
    vehicle_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictAddRequest {
    #[serde(flatten)]
    input: AddRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequest {
    vehicle_id: String,
    #[serde(flatten)]
    input: AddRequest,
}

#[derive(Deserialize)]
struct AddRequest {
    #[serde(default = "passenger")]
    class_id: String,
    make_id: String,
    model_id: String,
    year: i64,
    trim_id: Option<String>,
    vehicle: Value,
}

fn passenger() -> String {
    "passenger".into()
}

impl AddRequest {
    fn validate(&self) -> Result<String, RpcError> {
        nonempty(&self.class_id)?;
        if !self.vehicle.is_object()
            || self.year <= 0
            || self.vehicle["year"].as_i64() != Some(self.year)
        {
            return Err(RpcError::invalid_params());
        }
        for (section, id) in [
            (&self.vehicle["make"], &self.make_id),
            (&self.vehicle["model"], &self.model_id),
        ] {
            nonempty(id)?;
            let section_id = section["id"]
                .as_str()
                .ok_or_else(RpcError::invalid_params)?;
            nonempty(section_id)?;
            nonempty(
                section["name"]
                    .as_str()
                    .ok_or_else(RpcError::invalid_params)?,
            )?;
            if section_id != id || !section["raw_data"].is_object() {
                return Err(RpcError::invalid_params());
            }
            match (&self.trim_id, self.vehicle.get("trim")) {
                (None, None | Some(Value::Null)) => {}
                (Some(id), Some(section)) => {
                    nonempty(id)?;
                    let section_id = section["id"]
                        .as_str()
                        .ok_or_else(RpcError::invalid_params)?;
                    nonempty(section_id)?;
                    nonempty(
                        section["name"]
                            .as_str()
                            .ok_or_else(RpcError::invalid_params)?,
                    )?;
                    if section_id != id || !section["raw_data"].is_object() {
                        return Err(RpcError::invalid_params());
                    }
                }
                _ => return Err(RpcError::invalid_params()),
            }
        }
        for section in [
            "bodies",
            "engines",
            "mileages",
            "exterior_colors",
            "interior_colors",
        ] {
            let section = self.vehicle[section]
                .as_array()
                .ok_or_else(RpcError::invalid_params)?;
            if section.iter().any(|item| !item.is_object()) {
                return Err(RpcError::invalid_params());
            }
        }
        for value in [
            "selected_exterior_color",
            "selected_interior_color",
            "vin_data",
            "equipment_data",
        ] {
            if self
                .vehicle
                .get(value)
                .is_some_and(|v| !v.is_null() && !v.is_object())
            {
                return Err(RpcError::invalid_params());
            }
        }
        if self
            .vehicle
            .get("vin")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(RpcError::invalid_params());
        }
        Ok(format!(
            "{} {} · {}",
            self.vehicle["make"]["name"].as_str().unwrap(),
            self.vehicle["model"]["name"].as_str().unwrap(),
            self.year
        ))
    }

    fn record(self) -> Result<Document, RpcError> {
        let name = self.validate()?;
        Ok(doc! {
            "id": ObjectId::new().to_hex(),
            "name": name,
            "category": self.class_id,
            "verification": "none",
            "vehicle": json_to_bson(self.vehicle)?,
            "created_at": DateTime::now(),
        })
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|_| RpcError::invalid_params())
}

fn nonempty(value: &str) -> Result<(), RpcError> {
    if value.trim().is_empty() {
        Err(RpcError::invalid_params())
    } else {
        Ok(())
    }
}

// Convert literally: raw API objects with Extended JSON keys are not BSON types.
fn json_to_bson(value: Value) -> Result<Bson, RpcError> {
    Ok(match value {
        Value::Null => Bson::Null,
        Value::Bool(v) => Bson::Boolean(v),
        Value::String(v) => Bson::String(v),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                Bson::Int64(v)
            } else if v.is_f64() {
                Bson::Double(v.as_f64().ok_or_else(RpcError::invalid_params)?)
            } else {
                return Err(RpcError::invalid_params());
            }
        }
        Value::Array(items) => Bson::Array(
            items
                .into_iter()
                .map(json_to_bson)
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(items) => {
            let mut document = Document::new();
            for (key, value) in items {
                if key.contains('\0')
                    || [
                        "access_token",
                        "refresh_token",
                        "authorization",
                        "token",
                        "password",
                    ]
                    .iter()
                    .any(|credential| key.eq_ignore_ascii_case(credential))
                {
                    return Err(RpcError::invalid_params());
                }
                document.insert(key, json_to_bson(value)?);
            }
            Bson::Document(document)
        }
    })
}

fn bson_to_json(value: &Bson) -> Result<Value, RpcError> {
    Ok(match value {
        Bson::Null => Value::Null,
        Bson::Boolean(v) => json!(v),
        Bson::String(v) => json!(v),
        Bson::Int32(v) => json!(v),
        Bson::Int64(v) => json!(v),
        Bson::Double(v) if v.is_finite() => json!(v),
        Bson::Array(items) => {
            Value::Array(items.iter().map(bson_to_json).collect::<Result<_, _>>()?)
        }
        Bson::Document(items) => Value::Object(
            items
                .iter()
                .map(|(key, value)| Ok((key.clone(), bson_to_json(value)?)))
                .collect::<Result<_, RpcError>>()?,
        ),
        _ => return Err(RpcError::internal_error()),
    })
}

fn public_record(record: &Document) -> Result<Value, RpcError> {
    let mut result = serde_json::Map::new();
    for key in ["id", "name", "category", "verification", "vehicle"] {
        result.insert(
            key.into(),
            bson_to_json(record.get(key).ok_or_else(RpcError::internal_error)?)?,
        );
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests;
