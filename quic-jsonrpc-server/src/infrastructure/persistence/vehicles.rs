//! Public licensed catalog and private owner-bound garages use disjoint collections.
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use mongodb::{
    Client, Collection, Database, IndexModel,
    bson::{self, DateTime, Document, doc, oid::ObjectId},
    options::{IndexOptions, ReturnDocument},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    application::catalog_import::ParsedCatalog,
    domain::{
        RpcError, RpcResult,
        vehicle::{
            ARCHIVE_SHA256, AddVehicle, CATALOG_VERSION, CatalogEntity, CatalogPage, CatalogVariant, Garage, Vehicle,
            VehicleRepository, catalog_unavailable, garage_error, license, vehicle_not_found,
        },
    },
    infrastructure::config::MongoOptions,
};

const GARAGES: &str = "user_garages";
const META: &str = "catalog_vehicle_metadata";
const VARIANTS: &str = "catalog_vehicle_variants";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredVehicle {
    vehicle: Vehicle,
    client_request_id: String,
    request_hash: String,
    catalog_revision: String,
    source_path: String,
    added_at: DateTime,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GarageDocument {
    #[serde(rename = "_id")]
    id: ObjectId,
    owner: ObjectId,
    vehicles: Vec<StoredVehicle>,
    selected_vehicle_id: Option<String>,
    updated_at: DateTime,
}

pub struct MongoVehicleRepository {
    database: Database,
}

fn duplicate(error: &mongodb::error::Error, code: i32) -> bool {
    use mongodb::error::{ErrorKind, WriteFailure};
    match &*error.kind {
        ErrorKind::Command(e) => e.code == code,
        ErrorKind::Write(WriteFailure::WriteError(e)) => e.code == code,
        _ => false,
    }
}

async fn validated_collection(db: &Database, name: &str, validator: Document) -> Result<()> {
    match db.create_collection(name).validator(validator.clone()).await {
        Ok(_) => (),
        Err(e) if duplicate(&e, 48) => (),
        Err(_) => return Err(anyhow!("Unable to create vehicle collection")),
    }
    db.run_command(doc! {"collMod":name,"validator":validator,"validationLevel":"strict","validationAction":"error"})
        .await
        .map_err(|_| anyhow!("Unable to configure vehicle validator"))?;
    Ok(())
}

fn dimension_schema(max: f64) -> Document {
    doc! {"bsonType":"double","exclusiveMinimum":true,"minimum":0.0,"maximum":max}
}

impl MongoVehicleRepository {
    pub async fn connect(options: &MongoOptions) -> Result<Self> {
        let client = Client::with_uri_str(&options.uri)
            .await
            .map_err(|_| anyhow!("Unable to initialize vehicle database"))?;
        Self::from_database(client.database(&options.database)).await
    }

    pub async fn from_database(database: Database) -> Result<Self> {
        let vehicle = doc! {"bsonType":"object","additionalProperties":false,
        "required":["id","catalogVariantId","make","model","generation","engine","year","dimensions"],
        "properties":{
            "id":{"bsonType":"string","pattern":"^[0-9a-f]{24}$"},
            "catalogVariantId":{"bsonType":"string","pattern":"^variant-[0-9a-f]{64}$"},
            "make":{"bsonType":"string","maxLength":1024},"model":{"bsonType":"string","maxLength":1024},
            "generation":{"bsonType":"string","maxLength":1024},"engine":{"bsonType":"string","maxLength":1024},
            "year":{"bsonType":"int","minimum":1900,"maximum":2100},
            "dimensions":{"bsonType":"object","additionalProperties":false,"required":["height","width","length","mass"],
                "properties":{"height":dimension_schema(10.0),"width":dimension_schema(10.0),
                    "length":dimension_schema(50.0),"mass":dimension_schema(200.0)}}
        }};
        let stored = doc! {"bsonType":"object","additionalProperties":false,
        "required":["vehicle","clientRequestId","requestHash","catalogRevision","sourcePath","addedAt"],
        "properties":{"vehicle":vehicle,"clientRequestId":{"bsonType":"string","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"},
            "requestHash":{"bsonType":"string","pattern":"^[0-9a-f]{64}$"},
            "catalogRevision":{"bsonType":"string"},"sourcePath":{"bsonType":"string"},"addedAt":{"bsonType":"date"}}};
        validated_collection(
            &database,
            GARAGES,
            doc! {"$and":[
            {"$expr":{"$and":[
                {"$eq":["$_id","$owner"]},
                {"$eq":[{"$size":"$vehicles"},{"$size":{"$setUnion":[
                    {"$map":{"input":"$vehicles","as":"v","in":"$$v.clientRequestId"}},[]
                ]}}]},
                {"$or":[{"$eq":["$selectedVehicleId",null]},
                    {"$in":["$selectedVehicleId",{"$map":{"input":"$vehicles","as":"v","in":"$$v.vehicle.id"}}]}]}
            ]}},
                {"$jsonSchema":{"bsonType":"object","additionalProperties":false,
                    "required":["_id","owner","vehicles","selectedVehicleId","updatedAt"],
                    "properties":{"_id":{"bsonType":"objectId"},"owner":{"bsonType":"objectId"},
                        "vehicles":{"bsonType":"array","maxItems":100,"items":stored},
                        "selectedVehicleId":{"bsonType":["string","null"]},"updatedAt":{"bsonType":"date"}}}}
            ]},
        )
        .await?;
        let garage = database.collection::<Document>(GARAGES);
        garage
            .create_index(
                IndexModel::builder()
                    .keys(doc! {"owner":1})
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
            )
            .await?;
        garage
            .create_index(
                IndexModel::builder()
                    .keys(doc! {"owner":1,"vehicles.clientRequestId":1})
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
            )
            .await?;
        for kind in ["makes", "models", "generations", "variants"] {
            let name = collection_name(kind);
            let mut props = doc! {"_id":{"bsonType":"string"},"id":{"bsonType":"string"},
            "version":{"enum":[CATALOG_VERSION]},"sourcePath":{"bsonType":"string"},
            "yearStart":{"bsonType":["int","null"]},"yearEnd":{"bsonType":["int","null"]}};
            let mut required = vec!["_id", "id", "version", "sourcePath", "yearStart", "yearEnd"];
            if kind == "variants" {
                for key in [
                    "makeId",
                    "modelId",
                    "generationId",
                    "make",
                    "model",
                    "generation",
                    "engine",
                ] {
                    props.insert(key, doc! {"bsonType":"string"});
                    required.push(key);
                }
                for (key, max) in [
                    ("lengthMm", 50000.0),
                    ("widthMm", 10000.0),
                    ("heightMm", 10000.0),
                    ("curbWeightKg", 200000.0),
                ] {
                    props.insert(key, doc! {"bsonType":["double","null"],"minimum":1.0,"maximum":max});
                    required.push(key);
                }
            } else {
                props.insert("name", doc! {"bsonType":"string","minLength":1});
                props.insert("parentId", doc! {"bsonType":["string","null"]});
                required.extend(["name", "parentId"]);
            }
            validated_collection(&database, &name, doc! {"$and":[{"$expr":{"$eq":["$_id","$id"]}},
                {"$jsonSchema":{"bsonType":"object","additionalProperties":false,"required":required,"properties":props}}]}).await?;
            let collection = database.collection::<Document>(&name);
            collection
                .create_index(
                    IndexModel::builder()
                        .keys(doc! {"id":1})
                        .options(IndexOptions::builder().unique(true).build())
                        .build(),
                )
                .await?;
            collection
                .create_index(
                    IndexModel::builder()
                        .keys(if kind == "variants" {
                            doc! {"generationId":1,"id":1}
                        } else {
                            doc! {"parentId":1,"id":1}
                        })
                        .build(),
                )
                .await?;
        }
        Ok(Self { database })
    }

    fn garages(&self) -> Collection<GarageDocument> {
        self.database.collection(GARAGES)
    }

    async fn ready(&self) -> RpcResult<()> {
        let present = self
            .database
            .collection::<Document>(META)
            .find_one(doc! {"_id":CATALOG_VERSION,"ready":true,"archiveSha256":ARCHIVE_SHA256})
            .await
            .map_err(|_| catalog_unavailable())?;
        if present.is_none() {
            return Err(catalog_unavailable());
        }
        Ok(())
    }

    /// Called only by the explicit import CLI. The digest and complete schema are validated
    /// before this entry point. A readiness marker is written only after all public rows exist.
    pub async fn import(&self, catalog: &ParsedCatalog) -> Result<()> {
        let metadata = self.database.collection::<Document>(META);
        let counts = [
            ("makes", catalog.makes.len()),
            ("models", catalog.models.len()),
            ("generations", catalog.generations.len()),
            ("variants", catalog.variants.len()),
        ];
        let existing = metadata
            .find_one(doc! {"_id":CATALOG_VERSION,"ready":true,"archiveSha256":ARCHIVE_SHA256})
            .await?;
        if existing.is_some() {
            let mut complete = true;
            for (kind, count) in counts {
                complete &= self
                    .database
                    .collection::<Document>(&collection_name(kind))
                    .count_documents(doc! {})
                    .await?
                    == count as u64;
            }
            if complete {
                return Ok(());
            }
        }
        // The lock is operator-visible and never contains credentials. A failed import releases
        // it; a crashed process requires explicitly removing this single metadata lock document.
        metadata
            .insert_one(doc! {"_id":"import-lock","startedAt":DateTime::now()})
            .await
            .map_err(|_| {
                anyhow!("Catalog import already locked; inspect catalog_vehicle_metadata import-lock before retrying")
            })?;
        let result =
            async {
                metadata.delete_one(doc! {"_id":CATALOG_VERSION}).await?;
                self.import_rows("makes", &catalog.makes).await?;
                self.import_rows("models", &catalog.models).await?;
                self.import_rows("generations", &catalog.generations).await?;
                self.import_rows("variants", &catalog.variants).await?;
                for (kind, count) in counts {
                    ensure!(
                        self.database
                            .collection::<Document>(&collection_name(kind))
                            .count_documents(doc! {})
                            .await?
                            == count as u64,
                        "Incomplete catalog import"
                    );
                }
                metadata.insert_one(doc! {"_id":CATALOG_VERSION,"ready":true,"archiveSha256":ARCHIVE_SHA256,
                "importedAt":DateTime::now(),"makes":catalog.makes.len() as i64,"models":catalog.models.len() as i64,
                "generations":catalog.generations.len() as i64,"variants":catalog.variants.len() as i64,
                "invalidMeasurements":catalog.invalid_measurements as i64,
                "license":"ODbL-1.0","sourceUrl":crate::domain::vehicle::SOURCE_URL,
                "upstreamUrl":"https://www.autoevolution.com","schemaVersion":1,
                "units":{"lengthMm":"mm","widthMm":"mm","heightMm":"mm","curbWeightKg":"kg"}}).await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
        let unlock = metadata.delete_one(doc! {"_id":"import-lock"}).await;
        result?;
        unlock?;
        Ok(())
    }

    async fn import_rows<T: Serialize>(&self, kind: &str, rows: &[T]) -> Result<()> {
        let collection = self.database.collection::<Document>(&collection_name(kind));
        collection.delete_many(doc! {}).await?;
        for chunk in rows.chunks(500) {
            let mut docs = Vec::with_capacity(chunk.len());
            for row in chunk {
                let mut doc = bson::to_document(row)?;
                doc.insert("_id", doc.get_str("id")?.to_owned());
                docs.push(doc);
            }
            collection.insert_many(docs).await?;
        }
        Ok(())
    }

    async fn ensure_garage(&self, owner: ObjectId) -> RpcResult<()> {
        let document = GarageDocument {
            id: owner,
            owner,
            vehicles: vec![],
            selected_vehicle_id: None,
            updated_at: DateTime::now(),
        };
        match self
            .garages()
            .update_one(
                doc! {"_id":owner,"owner":owner},
                doc! {"$setOnInsert":bson::to_document(&document).map_err(|_| garage_error())?},
            )
            .upsert(true)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if duplicate(&e, 11000) => Ok(()),
            Err(_) => Err(garage_error()),
        }
    }
}

fn collection_name(kind: &str) -> String {
    format!("catalog_vehicle_{kind}")
}
fn owner_id(owner: &str) -> RpcResult<ObjectId> {
    ObjectId::parse_str(owner).map_err(|_| garage_error())
}
fn replay(garage: &GarageDocument, request: &str, hash: &str) -> RpcResult<Option<Value>> {
    match garage.vehicles.iter().find(|v| v.client_request_id == request) {
        Some(v) if v.request_hash == hash => Ok(Some(
            json!({"vehicle":v.vehicle,"selectedVehicleId":garage.selected_vehicle_id}),
        )),
        Some(_) => Err(RpcError::invalid_params()),
        None => Ok(None),
    }
}

#[async_trait]
impl VehicleRepository for MongoVehicleRepository {
    async fn resolve_owner(&self, normalized_login: &str) -> RpcResult<String> {
        let user = self
            .database
            .collection::<Document>("users")
            .find_one(doc! {"login_normalized":normalized_login,"enabled":true})
            .projection(doc! {"_id":1})
            .await
            .map_err(|_| garage_error())?
            .ok_or_else(RpcError::session_not_found)?;
        user.get_object_id("_id")
            .map(|id| id.to_hex())
            .map_err(|_| RpcError::session_not_found())
    }

    async fn catalog_page(&self, page: CatalogPage) -> RpcResult<Value> {
        self.ready().await?;
        let mut filter = doc! {};
        if let Some(parent) = &page.parent_id {
            filter.insert(
                if page.kind == "variants" {
                    "generationId"
                } else {
                    "parentId"
                },
                parent,
            );
        }
        if let Some(query) = page.query.filter(|q| !q.is_empty()) {
            let mut escaped = String::new();
            for c in query.chars() {
                if "\\.^$|?*+()[]{}".contains(c) {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            filter.insert("name", doc! {"$regex":escaped,"$options":"i"});
        }
        if let Some(cursor) = page.cursor {
            filter.insert("id", doc! {"$gt":cursor});
        }
        let mut cursor = self
            .database
            .collection::<Document>(&collection_name(&page.kind))
            .find(filter)
            .sort(doc! {"id":1})
            .limit(i64::from(page.limit) + 1)
            .await
            .map_err(|_| catalog_unavailable())?;
        let mut items = Vec::new();
        while cursor.advance().await.map_err(|_| catalog_unavailable())? {
            let doc = cursor.deserialize_current().map_err(|_| catalog_unavailable())?;
            let value = if page.kind == "variants" {
                let row: CatalogVariant = bson::from_document(doc).map_err(|_| catalog_unavailable())?;
                if page.export {
                    serde_json::to_value(row).map_err(|_| catalog_unavailable())?
                } else {
                    row.public()
                }
            } else {
                let row: CatalogEntity = bson::from_document(doc).map_err(|_| catalog_unavailable())?;
                if page.export {
                    serde_json::to_value(row).map_err(|_| catalog_unavailable())?
                } else if page.kind == "generations" {
                    json!({"id":row.id,"name":row.name,"yearStart":row.year_start,"yearEnd":row.year_end})
                } else {
                    json!({"id":row.id,"name":row.name})
                }
            };
            items.push(value);
        }
        let next = if items.len() > page.limit as usize {
            items.pop();
            items.last().and_then(|v| v.get("id")).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        if page.export {
            Ok(json!({"items":items,"nextCursor":next,"kind":page.kind,
            "version":CATALOG_VERSION,"schemaVersion":1,"license":license()}))
        } else {
            Ok(json!({"items":items,"nextCursor":next}))
        }
    }

    async fn variant(&self, id: &str) -> RpcResult<Option<CatalogVariant>> {
        self.ready().await?;
        self.database
            .collection::<CatalogVariant>(VARIANTS)
            .find_one(doc! {"_id":id})
            .await
            .map_err(|_| catalog_unavailable())
    }

    async fn garage(&self, owner: &str) -> RpcResult<Garage> {
        let owner = owner_id(owner)?;
        let garage = self
            .garages()
            .find_one(doc! {"_id":owner,"owner":owner})
            .await
            .map_err(|_| garage_error())?;
        Ok(match garage {
            Some(g) => Garage {
                vehicles: g.vehicles.into_iter().map(|v| v.vehicle).collect(),
                selected_vehicle_id: g.selected_vehicle_id,
            },
            None => Garage {
                vehicles: vec![],
                selected_vehicle_id: None,
            },
        })
    }

    async fn existing_add(&self, owner: &str, request_id: &str, hash: &str) -> RpcResult<Option<Value>> {
        let owner = owner_id(owner)?;
        match self
            .garages()
            .find_one(doc! {"_id":owner,"owner":owner})
            .await
            .map_err(|_| garage_error())?
        {
            Some(g) => replay(&g, request_id, hash),
            None => Ok(None),
        }
    }

    async fn add(&self, owner: &str, command: &AddVehicle, variant: &CatalogVariant, hash: &str) -> RpcResult<Value> {
        let owner_id = owner_id(owner)?;
        self.ensure_garage(owner_id).await?;
        let vehicle = Vehicle {
            id: ObjectId::new().to_hex(),
            catalog_variant_id: variant.id.clone(),
            make: variant.make.clone(),
            model: variant.model.clone(),
            generation: variant.generation.clone(),
            engine: variant.engine.clone(),
            year: command.year,
            dimensions: command.dimensions.clone(),
        };
        let entry = StoredVehicle {
            vehicle: vehicle.clone(),
            client_request_id: command.client_request_id.clone(),
            request_hash: hash.to_owned(),
            catalog_revision: format!("{CATALOG_VERSION}:{ARCHIVE_SHA256}"),
            source_path: variant.source_path.clone(),
            added_at: DateTime::now(),
        };
        let updated = self
            .garages()
            .find_one_and_update(
                doc! {"_id":owner_id,"owner":owner_id,
                "vehicles.clientRequestId":{"$ne":&command.client_request_id},
                "$expr":{"$lt":[{"$size":"$vehicles"},100]}},
                doc! {"$push":{"vehicles":bson::to_document(&entry).map_err(|_| garage_error())?},
                "$set":{"selectedVehicleId":&vehicle.id,"updatedAt":DateTime::now()}},
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| garage_error())?;
        if let Some(g) = updated {
            return Ok(json!({"vehicle":vehicle,"selectedVehicleId":g.selected_vehicle_id}));
        }
        if let Some(result) = self.existing_add(owner, &command.client_request_id, hash).await? {
            return Ok(result);
        }
        Err(RpcError::new(-32021, "Garage vehicle limit reached"))
    }

    async fn select(&self, owner: &str, vehicle_id: &str) -> RpcResult<String> {
        let owner = owner_id(owner)?;
        let result = self
            .garages()
            .find_one_and_update(
                doc! {"_id":owner,"owner":owner,"vehicles.vehicle.id":vehicle_id},
                doc! {"$set":{"selectedVehicleId":vehicle_id,"updatedAt":DateTime::now()}},
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| garage_error())?;
        if result.is_none() {
            return Err(vehicle_not_found());
        }
        Ok(vehicle_id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{AuthService, vehicle::VehicleService},
        domain::SessionRepository,
        infrastructure::persistence::{MongoAuthenticator, MongoSessionRepository},
        presentation::{RpcDispatcher, controller::StreamContext},
    };
    use std::{sync::Arc, time::Duration};

    const FIXTURE: &str = r#"{"makes":[{"name":"Fixture Make","models":[{"name":"Model","generations":[{"name":"Generation (2020)","yearStart":2020,"yearEnd":2024,"engines":[{"label":"Engine","lengthMm":4500,"widthMm":1800,"heightMm":1500,"curbWeightKg":1200},{"label":"Engine Unknown"}]}]}]},{"name":"Empty Make","models":[]}]}"#;
    const PASSWORD: &str = "5f4dcc3b5aa765d61d8327deb882cf99";

    async fn rpc(dispatcher: &RpcDispatcher, method: &str, params: Value) -> Value {
        dispatcher
            .handle_request(
                json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
                StreamContext {
                    connection_id: 1,
                    stream_id: 1,
                    remote_ip: "127.0.0.1".parse().unwrap(),
                },
            )
            .await
            .unwrap()
    }

    fn dispatcher(
        sessions: Arc<dyn SessionRepository>,
        auth: Arc<MongoAuthenticator>,
        repo: Arc<MongoVehicleRepository>,
    ) -> RpcDispatcher {
        let auth = Arc::new(AuthService::with_authenticator(
            sessions.clone(),
            auth,
            Duration::from_secs(60),
        ));
        crate::tests::dispatcher_with_auth(auth).with_vehicles(Arc::new(VehicleService::new(sessions, repo)))
    }

    #[tokio::test]
    #[ignore = "read-only configured Mongo check; run only after authorized pinned catalog import"]
    async fn configured_catalog_read_only_complete_public_export() {
        let values = dotenvy::from_path_iter(crate::env::crate_dir().join(".env"))
            .map_err(|_| "Unable to read configuration")
            .unwrap()
            .collect::<std::result::Result<std::collections::HashMap<_, _>, _>>()
            .map_err(|_| "Invalid configuration")
            .unwrap();
        let options = MongoOptions::from_lookup(|key| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| values.get(key).cloned().filter(|v| !v.is_empty()))
        })
        .map_err(|_| "Mongo configuration unavailable")
        .unwrap();
        let client = Client::with_uri_str(&options.uri)
            .await
            .map_err(|_| "Mongo unavailable")
            .unwrap();
        // Intentionally bypass schema/index preparation: this smoke test only reads public collections.
        let repository = MongoVehicleRepository {
            database: client.database(&options.database),
        };
        for (kind, expected) in [
            ("makes", 164),
            ("models", 2649),
            ("generations", 7217),
            ("variants", 30535),
        ] {
            let mut cursor = None;
            let mut ids = std::collections::HashSet::new();
            loop {
                let page = repository
                    .catalog_page(CatalogPage {
                        kind: kind.to_owned(),
                        parent_id: None,
                        query: None,
                        cursor,
                        limit: 100,
                        export: true,
                    })
                    .await
                    .unwrap();
                assert_eq!(page["license"]["archiveSha256"], ARCHIVE_SHA256);
                for row in page["items"].as_array().unwrap() {
                    let id = row["id"].as_str().unwrap();
                    assert!(ids.insert(id.to_owned()));
                    assert_eq!(row["version"], CATALOG_VERSION);
                    assert!(row["sourcePath"].as_str().unwrap().starts_with("json/"));
                    for key in [
                        "_id",
                        "owner",
                        "sessionId",
                        "clientRequestId",
                        "password_hash",
                        "vehicles",
                    ] {
                        assert!(row.get(key).is_none());
                    }
                }
                cursor = page["nextCursor"].as_str().map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(ids.len(), expected);
        }
    }

    #[tokio::test]
    #[ignore = "requires local MongoDB on 27017; uses and drops an isolated database"]
    async fn mongo_catalog_garage_dispatcher_ownership_and_persistence() {
        let options = MongoOptions {
            uri: "mongodb://127.0.0.1:27017/?serverSelectionTimeoutMS=1500".to_owned(),
            database: format!("brnavi_garage_test_{}", ObjectId::new().to_hex()),
        };
        let client = Client::with_uri_str(&options.uri).await.unwrap();
        let database = client.database(&options.database);
        let cleanup = database.clone();
        let result = tokio::spawn(async move {
            let auth = Arc::new(MongoAuthenticator::connect(&options,Duration::from_secs(60)).await.unwrap());
            let sessions = Arc::new(MongoSessionRepository::connect(&options,Duration::from_secs(60)).await.unwrap());
            let repo = Arc::new(MongoVehicleRepository::from_database(database.clone()).await.unwrap());
            let d = dispatcher(sessions.clone(),auth.clone(),repo.clone());
            assert_eq!(rpc(&d,"catalog.makes",json!({})).await["error"]["code"],-32020);
            assert_eq!(rpc(&d,"catalog.export",json!({})).await["error"]["code"],-32020);
            assert_eq!(rpc(&d,"catalog.license",json!({})).await["result"]["export"]["method"],"catalog.export");
            for email in ["owner@example.com","other@example.com"] {
                assert_eq!(rpc(&d,"createAccount",json!({"email":email,"name":"Fixture Driver","password":PASSWORD})).await["result"]["created"],true);
            }
            let login = rpc(&d,"login",json!({"login":" OWNER@EXAMPLE.COM ","password":PASSWORD})).await;
            let sid = login["result"]["session"]["sessionId"].as_str().unwrap().to_owned();
            let second = rpc(&d,"login",json!({"login":"other@example.com","password":PASSWORD})).await;
            let other_sid = second["result"]["session"]["sessionId"].as_str().unwrap().to_owned();
            assert_eq!(sessions.find(&sid).await.unwrap().unwrap().principal.phone,"owner@example.com");
            let users = database.collection::<Document>("users");
            let profiles = database.collection::<Document>("profile");
            let original_user = users.find_one(doc! {"login_normalized":"owner@example.com"}).await.unwrap().unwrap();
            let owner = original_user.get_object_id("_id").unwrap();
            let original_profile = profiles.find_one(doc! {"user_id":owner}).await.unwrap().unwrap();
            assert_eq!(rpc(&d,"garage.list",json!({"sessionId":sid})).await["result"],json!({"vehicles":[],"selectedVehicleId":null}));
            assert_eq!(rpc(&d,"garage.list",json!({"sessionId":"forged"})).await["error"]["code"],-32001);
            assert_eq!(rpc(&d,"garage.list",json!({"sessionId":sid,"userId":owner.to_hex()})).await["error"]["code"],-32602);

            let parsed = ParsedCatalog::fixture(FIXTURE).unwrap();
            repo.import(&parsed).await.unwrap();
            let makes = rpc(&d,"catalog.makes",json!({"query":"fixture","limit":1})).await;
            let make_id = makes["result"]["items"][0]["id"].as_str().unwrap();
            let models = rpc(&d,"catalog.models",json!({"makeId":make_id})).await;
            let model_id = models["result"]["items"][0]["id"].as_str().unwrap();
            let generations = rpc(&d,"catalog.generations",json!({"modelId":model_id})).await;
            let generation_id = generations["result"]["items"][0]["id"].as_str().unwrap();
            assert_eq!(generations["result"]["items"][0]["yearStart"],2020);
            let variants = rpc(&d,"catalog.variants",json!({"generationId":generation_id,"limit":1})).await;
            assert_eq!(variants["result"]["items"].as_array().unwrap().len(),1);
            let cursor = variants["result"]["nextCursor"].as_str().unwrap();
            let page2 = rpc(&d,"catalog.variants",json!({"generationId":generation_id,"limit":1,"cursor":cursor})).await;
            assert_eq!(page2["result"]["nextCursor"],Value::Null);
            assert_ne!(variants["result"]["items"][0]["id"],page2["result"]["items"][0]["id"]);
            assert!(page2["result"]["items"][0]["heightMm"].is_null() || variants["result"]["items"][0]["heightMm"].is_null());
            assert_eq!(rpc(&d,"catalog.makes",json!({"query":"["})).await["result"]["items"],json!([]));
            for (method,p) in [("catalog.makes",json!({"limit":101})),("catalog.makes",json!({"limit":0})),
                ("catalog.models",json!({})),("catalog.models",json!({"makeId":"forged"})),
                ("catalog.generations",json!({"modelId":model_id,"query":"x"})),
                ("catalog.export",json!({"kind":"users"})),("catalog.export",json!({"sessionId":sid})),
                ("catalog.makes",json!({"cursor":"bad"}))] {
                assert_eq!(rpc(&d,method,p).await["error"]["code"],-32602);
            }
            let variant_id = parsed.variants[0].id.clone();
            let mut command = json!({"sessionId":sid,"catalogVariantId":variant_id,"year":2021,
                "dimensions":{"height":1.6,"width":1.9,"length":4.6,"mass":1.5},
                "clientRequestId":"c5b95d32-a56c-4709-b228-162b9b4e4368"});
            let original_command = command.clone();
            command["year"] = json!(2019);
            assert_eq!(rpc(&d,"garage.add",command.clone()).await["error"]["code"],-32602);
            command = original_command.clone(); command["dimensions"]["mass"] = json!(201.0);
            assert_eq!(rpc(&d,"garage.add",command.clone()).await["error"]["code"],-32602);
            command = original_command.clone(); command.as_object_mut().unwrap().remove("clientRequestId");
            assert_eq!(rpc(&d,"garage.add",command).await["error"]["code"],-32602);
            let (a,b) = tokio::join!(rpc(&d,"garage.add",original_command.clone()),rpc(&d,"garage.add",original_command.clone()));
            assert_eq!(a["result"],b["result"]);
            let vehicle_id = a["result"]["vehicle"]["id"].as_str().unwrap().to_owned();
            assert_eq!(a["result"]["selectedVehicleId"],vehicle_id);
            assert_eq!(a["result"]["vehicle"]["dimensions"]["mass"],1.5);
            assert_eq!(rpc(&d,"garage.list",json!({"sessionId":sid})).await["result"]["vehicles"].as_array().unwrap().len(),1);
            command = original_command.clone(); command["year"]=json!(2022);
            assert_eq!(rpc(&d,"garage.add",command).await["error"]["code"],-32602);
            command = original_command.clone(); command["clientRequestId"]=json!("0c6c62f4-4979-4d95-9fa5-c0a6c66c1aef");
            let added = rpc(&d,"garage.add",command).await;
            assert_ne!(added["result"]["vehicle"]["id"],vehicle_id);
            assert_eq!(rpc(&d,"garage.select",json!({"sessionId":sid,"vehicleId":vehicle_id})).await["result"]["selectedVehicleId"],vehicle_id);
            assert_eq!(rpc(&d,"garage.add",original_command.clone()).await["result"]["selectedVehicleId"],vehicle_id);
            assert_eq!(rpc(&d,"garage.list",json!({"sessionId":other_sid})).await["result"],json!({"vehicles":[],"selectedVehicleId":null}));
            for id in [&vehicle_id,"000000000000000000000000"] {
                assert_eq!(rpc(&d,"garage.select",json!({"sessionId":other_sid,"vehicleId":id})).await["error"]["code"],-32022);
            }
            let other_command = { let mut p=original_command.clone();p["sessionId"]=json!(other_sid);p };
            let other_added = rpc(&d,"garage.add",other_command).await;
            assert_ne!(other_added["result"]["vehicle"]["id"],vehicle_id);
            let reloaded = dispatcher(Arc::new(MongoSessionRepository::connect(&options,Duration::from_secs(60)).await.unwrap()),
                auth.clone(),Arc::new(MongoVehicleRepository::from_database(database.clone()).await.unwrap()));
            let reload = rpc(&reloaded,"garage.list",json!({"sessionId":sid})).await;
            assert_eq!(reload["result"]["selectedVehicleId"],vehicle_id);
            assert_eq!(reload["result"]["vehicles"].as_array().unwrap().len(),2);
            let raw_garages = database.collection::<Document>(GARAGES);
            let raw_garage = raw_garages.find_one(doc! {"_id":owner}).await.unwrap().unwrap();
            let duplicate_entry = raw_garage.get_array("vehicles").unwrap()[0].clone();
            assert!(raw_garages.update_one(doc! {"_id":owner},doc! {"$push":{"vehicles":duplicate_entry}}).await.is_err());
            assert!(raw_garages.update_one(doc! {"_id":owner},doc! {"$set":{"selectedVehicleId":"000000000000000000000000"}}).await.is_err());

            for (kind,count) in [("makes",2),("models",1),("generations",1),("variants",2)] {
                let mut cursor = Value::Null;
                let mut seen = 0;
                loop {
                    let mut params=json!({"kind":kind,"limit":1});
                    if !cursor.is_null() { params["cursor"]=cursor; }
                    let page=rpc(&reloaded,"catalog.export",params).await;
                    assert!(page.get("error").is_none(),"{page}");
                    let text=page.to_string();
                    for secret in [&sid,&other_sid,PASSWORD,"password_hash","clientRequestId","owner@example.com","owner\""] {
                        assert!(!text.contains(secret),"public catalog leaked private data");
                    }
                    assert_eq!(page["result"]["license"]["name"],"ODbL-1.0");
                    seen+=page["result"]["items"].as_array().unwrap().len();
                    cursor=page["result"]["nextCursor"].clone(); if cursor.is_null() { break; }
                }
                assert_eq!(seen,count);
            }
            assert_eq!(users.find_one(doc! {"_id":owner}).await.unwrap().unwrap(),original_user);
            assert_eq!(profiles.find_one(doc! {"user_id":owner}).await.unwrap().unwrap(),original_profile);
            // A storage validation failure must not insert without selecting (or vice versa).
            database.run_command(doc! {"collMod":GARAGES,"validator":{"vehicles":{"$size":2}}}).await.unwrap();
            command=original_command.clone();command["clientRequestId"]=json!("6e6064e9-a308-4b89-b4ba-e448753db28a");
            assert_eq!(rpc(&reloaded,"garage.add",command).await["error"]["code"],-32021);
            assert_eq!(rpc(&reloaded,"garage.list",json!({"sessionId":sid})).await["result"],reload["result"]);
            MongoVehicleRepository::from_database(database.clone()).await.unwrap();
            // Parallel new requests cannot exceed the strict 100-vehicle bound.
            let mut handles=vec![];
            for i in 0..105 {
                let repo=repo.clone();let owner=owner.to_hex();let variant=parsed.variants[0].clone();
                let p: AddVehicle=serde_json::from_value(json!({"sessionId":sid,"catalogVariantId":variant.id,"year":2021,
                    "dimensions":{"height":1.6,"width":1.9,"length":4.6,"mass":1.5},
                    "clientRequestId":format!("00000000-0000-4000-8000-{i:012x}")})).unwrap();
                handles.push(tokio::spawn(async move {repo.add(&owner,&p,&variant,&format!("{i:064x}")).await}));
            }
            let mut success=0;
            for handle in handles { success+=usize::from(handle.await.unwrap().is_ok()); }
            assert_eq!(success,98);
            assert_eq!(repo.garage(&owner.to_hex()).await.unwrap().vehicles.len(),100);
            assert_eq!(rpc(&reloaded,"garage.add",original_command).await["result"]["vehicle"]["id"],vehicle_id);
            users.update_one(doc! {"_id":owner},doc! {"$set":{"enabled":false}}).await.unwrap();
            assert_eq!(rpc(&reloaded,"garage.list",json!({"sessionId":sid})).await["error"]["code"],-32001);
            sessions.remove(&other_sid).await.unwrap();
            assert_eq!(rpc(&reloaded,"garage.list",json!({"sessionId":other_sid})).await["error"]["code"],-32001);
        }).await;
        cleanup.drop().await.expect("drop isolated garage database");
        result.expect("catalog and garage integration assertions");
    }
}
