use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::RpcResult;

pub const CATALOG_VERSION: &str = "v1.0.2";
pub const ARCHIVE_SHA256: &str = "76e524ef5b26c733d2ec911df706f0dec0dcc28f4032f261f0ecf139ba10db8e";
pub const SOURCE_URL: &str = "https://github.com/gor3a/vehicle-makes-models";
pub const LICENSE_URL: &str = "https://opendatacommons.org/licenses/odbl/1-0/";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dimensions {
    pub height: f64,
    pub width: f64,
    pub length: f64,
    pub mass: f64,
}

impl Dimensions {
    pub fn valid(&self) -> bool {
        [
            (self.height, 10.0),
            (self.width, 10.0),
            (self.length, 50.0),
            (self.mass, 200.0),
        ]
        .into_iter()
        .all(|(value, max)| value.is_finite() && value > 0.0 && value <= max)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Vehicle {
    pub id: String,
    pub catalog_variant_id: String,
    pub make: String,
    pub model: String,
    pub generation: String,
    pub engine: String,
    pub year: i32,
    pub dimensions: Dimensions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogVariant {
    pub id: String,
    pub make_id: String,
    pub model_id: String,
    pub generation_id: String,
    pub make: String,
    pub model: String,
    pub generation: String,
    pub engine: String,
    pub year_start: Option<i32>,
    pub year_end: Option<i32>,
    pub length_mm: Option<f64>,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub curb_weight_kg: Option<f64>,
    pub source_path: String,
    pub version: String,
}

impl CatalogVariant {
    pub fn public(&self) -> Value {
        serde_json::json!({
            "id": self.id, "make": self.make, "model": self.model,
            "generation": self.generation, "engine": self.engine,
            "yearStart": self.year_start, "yearEnd": self.year_end,
            "lengthMm": self.length_mm, "widthMm": self.width_mm,
            "heightMm": self.height_mm, "curbWeightKg": self.curb_weight_kg
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntity {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub year_start: Option<i32>,
    pub year_end: Option<i32>,
    pub source_path: String,
    pub version: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Garage {
    pub vehicles: Vec<Vehicle>,
    pub selected_vehicle_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddVehicle {
    pub session_id: String,
    pub catalog_variant_id: String,
    pub year: i32,
    pub dimensions: Dimensions,
    pub client_request_id: String,
}

#[derive(Clone)]
pub struct CatalogPage {
    pub kind: String,
    pub parent_id: Option<String>,
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
    pub export: bool,
}

// Owner handles are resolved from enabled database users, never from request parameters.
#[async_trait]
pub trait VehicleRepository: Send + Sync {
    async fn resolve_owner(&self, normalized_login: &str) -> RpcResult<String>;
    async fn catalog_page(&self, page: CatalogPage) -> RpcResult<Value>;
    async fn variant(&self, id: &str) -> RpcResult<Option<CatalogVariant>>;
    async fn garage(&self, owner: &str) -> RpcResult<Garage>;
    async fn existing_add(&self, owner: &str, request_id: &str, hash: &str) -> RpcResult<Option<Value>>;
    async fn add(&self, owner: &str, command: &AddVehicle, variant: &CatalogVariant, hash: &str) -> RpcResult<Value>;
    async fn select(&self, owner: &str, vehicle_id: &str) -> RpcResult<String>;
}

pub fn catalog_unavailable() -> crate::domain::RpcError {
    crate::domain::RpcError::new(-32020, "Vehicle catalog unavailable")
}

pub fn garage_error() -> crate::domain::RpcError {
    crate::domain::RpcError::new(-32021, "Garage operation unavailable")
}

pub fn vehicle_not_found() -> crate::domain::RpcError {
    crate::domain::RpcError::new(-32022, "Vehicle not found")
}

pub fn license() -> Value {
    serde_json::json!({
        "name": "ODbL-1.0", "licenseUrl": LICENSE_URL, "sourceUrl": SOURCE_URL,
        "upstreamUrl": "https://www.autoevolution.com", "version": CATALOG_VERSION,
        "archiveSha256": ARCHIVE_SHA256,
        "attribution": "Vehicle data from vehicle-makes-models (Mina Sameh / gor3a), originally sourced from autoevolution.com; adapted by BRNavi. Available under ODbL 1.0.",
        "export": {"method": "catalog.export", "authenticationRequired": false,
            "kinds": ["makes", "models", "generations", "variants"],
            "instructions": "For each kind, request catalog.export {kind,limit:100}; repeat with nextCursor until null. The complete adapted database is available through these public paginated RPCs.",
            "schemaVersion": 1},
        "units": {"lengthMm": "millimetres", "widthMm": "millimetres", "heightMm": "millimetres", "curbWeightKg": "kilograms"},
        "adaptation": "Stable SHA-256 IDs from pinned archive JSON paths; dimensions outside plausible positive ranges become null; omitted source values remain null. Original archive retained by operator. No private garage data is part of this database."
    })
}
