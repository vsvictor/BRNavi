use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    application::catalog_import::sha256,
    domain::{
        RpcError, RpcResult, SessionRepository, normalize_login,
        vehicle::{AddVehicle, CatalogPage, VehicleRepository, license, vehicle_not_found},
    },
};

pub struct VehicleService {
    sessions: Arc<dyn SessionRepository>,
    repository: Arc<dyn VehicleRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageParams {
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    make_id: Option<String>,
    model_id: Option<String>,
    generation_id: Option<String>,
    kind: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionParams {
    session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectParams {
    session_id: String,
    vehicle_id: String,
}

fn parse<T: serde::de::DeserializeOwned>(params: Option<Value>) -> RpcResult<T> {
    serde_json::from_value(params.unwrap_or_else(|| json!({}))).map_err(|_| RpcError::invalid_params())
}

fn valid_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

pub fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
        && value != "00000000-0000-0000-0000-000000000000"
}

pub fn current_year() -> i32 {
    let mut days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    let mut year = 1970;
    loop {
        let n = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if days < n {
            return year;
        }
        days -= n;
        year += 1;
    }
}

impl VehicleService {
    pub fn new(sessions: Arc<dyn SessionRepository>, repository: Arc<dyn VehicleRepository>) -> Self {
        Self { sessions, repository }
    }

    async fn owner(&self, session_id: &str) -> RpcResult<String> {
        if session_id.is_empty() || session_id.len() > 256 {
            return Err(RpcError::invalid_params());
        }
        let session = self
            .sessions
            .find(session_id)
            .await
            .map_err(|_| RpcError::session_storage())?
            .ok_or_else(RpcError::session_not_found)?;
        let login = normalize_login(&session.principal.phone).map_err(|_| RpcError::session_not_found())?;
        self.repository.resolve_owner(&login).await
    }

    pub async fn handle(&self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "catalog.license" => {
                let p: Value = params.unwrap_or_else(|| json!({}));
                if p.as_object().is_none_or(|o| !o.is_empty()) {
                    return Err(RpcError::invalid_params());
                }
                Ok(license())
            }
            "catalog.makes" | "catalog.models" | "catalog.generations" | "catalog.variants" | "catalog.export" => {
                let p: PageParams = parse(params)?;
                let export = method == "catalog.export";
                let kind = if export {
                    p.kind.clone().unwrap_or_else(|| "variants".to_owned())
                } else {
                    method.trim_start_matches("catalog.").to_owned()
                };
                let prefix = match kind.as_str() {
                    "makes" => "make-",
                    "models" => "model-",
                    "generations" => "generation-",
                    "variants" => "variant-",
                    _ => return Err(RpcError::invalid_params()),
                };
                let parent = match kind.as_str() {
                    "models" if !export => p.make_id.clone(),
                    "generations" if !export => p.model_id.clone(),
                    "variants" if !export => p.generation_id.clone(),
                    _ => None,
                };
                if !export && kind != "makes" && parent.is_none() {
                    return Err(RpcError::invalid_params());
                }
                if p.make_id.is_some() != (!export && kind == "models")
                    || p.model_id.is_some() != (!export && kind == "generations")
                    || p.generation_id.is_some() != (!export && kind == "variants")
                    || (!export && p.kind.is_some())
                {
                    return Err(RpcError::invalid_params());
                }
                if let Some(id) = parent.as_ref() {
                    let pp = match kind.as_str() {
                        "models" => "make-",
                        "generations" => "model-",
                        _ => "generation-",
                    };
                    if !valid_id(id, pp) {
                        return Err(RpcError::invalid_params());
                    }
                }
                if p.query.as_ref().is_some_and(|q| q.len() > 100)
                    || (p.query.is_some() && (export || !matches!(kind.as_str(), "makes" | "models")))
                {
                    return Err(RpcError::invalid_params());
                }
                if p.cursor.as_ref().is_some_and(|c| !valid_id(c, prefix)) {
                    return Err(RpcError::invalid_params());
                }
                let limit = p.limit.unwrap_or(50);
                if !(1..=100).contains(&limit) {
                    return Err(RpcError::invalid_params());
                }
                self.repository
                    .catalog_page(CatalogPage {
                        kind,
                        parent_id: parent,
                        query: p.query,
                        cursor: p.cursor,
                        limit,
                        export,
                    })
                    .await
            }
            "garage.list" => {
                let p: SessionParams = parse(params)?;
                let owner = self.owner(&p.session_id).await?;
                serde_json::to_value(self.repository.garage(&owner).await?)
                    .map_err(|_| crate::domain::vehicle::garage_error())
            }
            "garage.add" => {
                let mut p: AddVehicle = parse(params)?;
                if !p.dimensions.valid()
                    || !valid_uuid(&p.client_request_id)
                    || !valid_id(&p.catalog_variant_id, "variant-")
                    || !(1900..=current_year() + 1).contains(&p.year)
                {
                    return Err(RpcError::invalid_params());
                }
                p.client_request_id.make_ascii_lowercase();
                let owner = self.owner(&p.session_id).await?;
                let hash = sha256(
                    serde_json::to_vec(&json!({"variant":p.catalog_variant_id,
                    "year":p.year,"dimensions":p.dimensions}))
                    .map_err(|_| RpcError::invalid_params())?
                    .as_slice(),
                );
                // Replays remain valid even if catalog storage is temporarily unavailable.
                if let Some(result) = self
                    .repository
                    .existing_add(&owner, &p.client_request_id, &hash)
                    .await?
                {
                    return Ok(result);
                }
                let variant = self
                    .repository
                    .variant(&p.catalog_variant_id)
                    .await?
                    .ok_or_else(vehicle_not_found)?;
                if variant.year_start.is_some_and(|year| p.year < year)
                    || variant.year_end.is_some_and(|year| p.year > year)
                {
                    return Err(RpcError::invalid_params());
                }
                self.repository.add(&owner, &p, &variant, &hash).await
            }
            "garage.select" => {
                let p: SelectParams = parse(params)?;
                if p.vehicle_id.len() != 24 || !p.vehicle_id.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return Err(RpcError::invalid_params());
                }
                let owner = self.owner(&p.session_id).await?;
                Ok(
                    json!({"selectedVehicleId":self.repository.select(&owner, &p.vehicle_id.to_ascii_lowercase()).await?}),
                )
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::vehicle::Dimensions;
    #[test]
    fn finite_positive_dimensions_uuid_and_ids() {
        let mut d = Dimensions {
            height: 1.5,
            width: 2.0,
            length: 5.0,
            mass: 1.2,
        };
        assert!(d.valid());
        for mass in [0.0, -1.0, 201.0, f64::NAN, f64::INFINITY] {
            d.mass = mass;
            assert!(!d.valid());
        }
        assert!(valid_uuid("c5b95d32-a56c-4709-b228-162b9b4e4368"));
        assert!(!valid_uuid("owner-supplied"));
        assert!(!valid_uuid("00000000-0000-0000-0000-000000000000"));
        assert!(!valid_id("variant-forged", "variant-"));
        assert!((2026..2100).contains(&current_year()));
    }
}
