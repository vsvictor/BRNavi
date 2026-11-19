use mongodb::{
    Collection, Database,
    bson::{Bson, DateTime, Document, RawDocumentBuf, doc},
    options::ReturnDocument,
};

use super::{GarageState, GarageStore, RpcError};

#[derive(Clone)]
pub struct MongoGarage {
    collection: Collection<Document>,
}

impl MongoGarage {
    pub fn new(database: Database) -> Self {
        Self {
            collection: database.collection("garage"),
        }
    }
}

impl GarageStore for MongoGarage {
    async fn add(&self, owner: String, vehicle: Document) -> Result<(), RpcError> {
        // The unique _id index serializes owner upserts. A losing initial upsert
        // retries without upsert against the newly created owner document.
        let (filter, update) = add_query(owner, vehicle)?;
        match self
            .collection
            .update_one(filter.clone(), update.clone())
            .upsert(true)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if duplicate_key(&error) => {
                let result = self
                    .collection
                    .update_one(filter, update)
                    .await
                    .map_err(storage_error)?;
                if result.matched_count != 1 {
                    return Err(RpcError::internal_error());
                }
                Ok(())
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    async fn list(&self, owner: String) -> Result<GarageState, RpcError> {
        let Some(document) = self
            .collection
            .clone_with_type::<RawDocumentBuf>()
            .find_one(doc! { "_id": owner })
            .await
            .map_err(storage_error)?
        else {
            return Ok(GarageState::default());
        };
        // Avoid Serde's Extended JSON interpretation of literal raw API keys.
        let document = document.to_document().map_err(storage_error)?;
        let vehicles = document
            .get_array("vehicles")
            .map_err(storage_error)?
            .iter()
            .map(|value| match value {
                Bson::Document(record) => {
                    validate_record(record)?;
                    Ok(record.clone())
                }
                _ => Err(RpcError::internal_error()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let active_vehicle_id = match document.get("active_vehicle_id") {
            Some(Bson::Null) if vehicles.is_empty() => None,
            Some(Bson::String(id))
                if vehicles.iter().any(|record| record.get_str("id") == Ok(id)) =>
            {
                Some(id.clone())
            }
            _ => return Err(RpcError::internal_error()),
        };
        Ok(GarageState {
            vehicles,
            active_vehicle_id,
        })
    }

    async fn set_active(&self, owner: String, vehicle_id: String) -> Result<bool, RpcError> {
        let (filter, update) = active_query(owner, vehicle_id);
        let result = self
            .collection
            .update_one(filter, update)
            .await
            .map_err(storage_error)?;
        Ok(result.matched_count == 1)
    }

    async fn update(
        &self,
        owner: String,
        vehicle_id: String,
        vehicle: Document,
    ) -> Result<Option<Document>, RpcError> {
        validate_record(&vehicle)?;
        let (filter, update) = update_query(owner, vehicle_id.clone(), vehicle);
        let result = self
            .collection
            .clone_with_type::<RawDocumentBuf>()
            .find_one_and_update(filter, update)
            .return_document(ReturnDocument::After)
            .await
            .map_err(storage_error)?;
        let Some(document) = result else {
            return Ok(None);
        };
        let document = document.to_document().map_err(storage_error)?;
        let record = document
            .get_array("vehicles")
            .map_err(storage_error)?
            .iter()
            .filter_map(Bson::as_document)
            .find(|record| record.get_str("id") == Ok(vehicle_id.as_str()))
            .ok_or_else(RpcError::internal_error)?;
        validate_record(record)?;
        Ok(Some(record.clone()))
    }

    async fn delete(
        &self,
        owner: String,
        vehicle_id: String,
    ) -> Result<Option<Option<String>>, RpcError> {
        let (filter, update) = delete_query(owner, vehicle_id);
        let result = self
            .collection
            .clone_with_type::<RawDocumentBuf>()
            .find_one_and_update(filter, update)
            .return_document(ReturnDocument::After)
            .await
            .map_err(storage_error)?;
        let Some(document) = result else {
            return Ok(None);
        };
        let document = document.to_document().map_err(storage_error)?;
        match document.get("active_vehicle_id") {
            Some(Bson::String(id)) => Ok(Some(Some(id.clone()))),
            Some(Bson::Null) => Ok(Some(None)),
            _ => Err(RpcError::internal_error()),
        }
    }
}

fn add_query(owner: String, vehicle: Document) -> Result<(Document, Vec<Document>), RpcError> {
    super::nonempty(&owner).map_err(storage_error)?;
    validate_record(&vehicle)?;
    let id = vehicle.get_str("id").map_err(storage_error)?.to_owned();
    Ok((
        doc! { "_id": owner },
        vec![doc! { "$set": {
            "vehicles": { "$concatArrays": [
                { "$ifNull": ["$vehicles", []] }, [{ "$literal": vehicle }]
            ] },
            "active_vehicle_id": { "$cond": [
                { "$eq": [{ "$size": { "$ifNull": ["$vehicles", []] } }, 0] },
                { "$literal": id }, "$active_vehicle_id"
            ] },
            "created_at": { "$ifNull": ["$created_at", DateTime::now()] },
            "updated_at": DateTime::now(),
        }}],
    ))
}

fn update_query(owner: String, vehicle_id: String, vehicle: Document) -> (Document, Document) {
    (
        doc! { "_id": owner, "vehicles.id": vehicle_id },
        doc! { "$set": {
            "vehicles.$.name": vehicle.get("name").unwrap().clone(),
            "vehicles.$.category": vehicle.get("category").unwrap().clone(),
            "vehicles.$.vehicle": vehicle.get("vehicle").unwrap().clone(),
            "updated_at": DateTime::now(),
        }},
    )
}

fn delete_query(owner: String, vehicle_id: String) -> (Document, Vec<Document>) {
    (
        doc! { "_id": owner, "vehicles.id": &vehicle_id },
        vec![
            doc! { "$set": {
                "vehicles": { "$filter": {
                    "input": "$vehicles", "as": "vehicle",
                    "cond": { "$ne": ["$$vehicle.id", { "$literal": &vehicle_id }] },
                } },
                "updated_at": DateTime::now(),
            }},
            doc! { "$set": {
                "active_vehicle_id": { "$cond": [
                    { "$eq": ["$active_vehicle_id", { "$literal": vehicle_id }] },
                    { "$ifNull": [{ "$arrayElemAt": ["$vehicles.id", 0] }, Bson::Null] },
                    "$active_vehicle_id"
                ] },
            }},
        ],
    )
}

fn validate_record(record: &Document) -> Result<(), RpcError> {
    let public = super::public_record(record)?;
    let snapshot = &public["vehicle"];
    let input: super::AddRequest = super::parse(serde_json::json!({
        "class_id": public["category"],
        "make_id": snapshot["make"]["id"],
        "model_id": snapshot["model"]["id"],
        "year": snapshot["year"],
        "trim_id": snapshot["trim"]["id"],
        "vehicle": snapshot,
    }))
    .map_err(storage_error)?;
    let name = input.validate().map_err(storage_error)?;
    super::json_to_bson(input.vehicle).map_err(storage_error)?;
    super::nonempty(public["id"].as_str().ok_or_else(RpcError::internal_error)?)
        .map_err(storage_error)?;
    if public["verification"] != "none"
        || public["name"] != name
        || !matches!(record.get("created_at"), Some(Bson::DateTime(_)))
    {
        return Err(RpcError::internal_error());
    }
    Ok(())
}

fn active_query(owner: String, vehicle_id: String) -> (Document, Document) {
    (
        doc! { "_id": owner, "vehicles.id": &vehicle_id },
        doc! { "$set": { "active_vehicle_id": vehicle_id, "updated_at": DateTime::now() } },
    )
}

fn duplicate_key(error: &mongodb::error::Error) -> bool {
    matches!(
        error.kind.as_ref(),
        mongodb::error::ErrorKind::Write(mongodb::error::WriteFailure::WriteError(error))
            if error.code == 11000
    )
}

fn storage_error<T>(_error: T) -> RpcError {
    RpcError::internal_error()
}

#[cfg(test)]
mod tests;
