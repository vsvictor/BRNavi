use super::*;

fn record() -> Document {
    let input = serde_json::json!({
        "make_id": "17", "model_id": "27", "year": 2024, "trim_id": "37",
        "vehicle": crate::garage::tests::snapshot(),
    });
    super::super::parse::<super::super::AddRequest>(input)
        .unwrap()
        .record()
        .unwrap()
}

#[test]
fn mongo_accepts_record_without_trim() {
    let mut snapshot = crate::garage::tests::snapshot();
    snapshot["trim"] = serde_json::Value::Null;
    let input = serde_json::json!({
        "make_id": "17", "model_id": "27", "year": 2024, "vehicle": snapshot,
    });
    let vehicle = super::super::parse::<super::super::AddRequest>(input)
        .unwrap()
        .record()
        .unwrap();
    assert!(add_query("verified-user".into(), vehicle).is_ok());
}

#[tokio::test]
async fn collection_is_exactly_garage_without_connecting_to_a_database() {
    let client = mongodb::Client::with_uri_str("mongodb://127.0.0.1:27017")
        .await
        .unwrap();
    let store = MongoGarage::new(client.database("garage-test-not-connected"));
    assert_eq!(store.collection.name(), "garage");
}

#[test]
fn mongo_add_query_appends_literal_snapshot_and_selects_on_empty() {
    let mut vehicle = record();
    vehicle.insert("id", "car-1");
    let (filter, update) = add_query("verified-user".into(), vehicle.clone()).unwrap();
    assert_eq!(filter, doc! { "_id": "verified-user" });
    let set = update[0].get_document("$set").unwrap();
    let concat = set
        .get_document("vehicles")
        .unwrap()
        .get_array("$concatArrays")
        .unwrap();
    assert_eq!(
        concat[1].as_array().unwrap()[0]
            .as_document()
            .unwrap()
            .get_document("$literal")
            .unwrap(),
        &vehicle
    );
    assert_eq!(
        set.get_document("active_vehicle_id")
            .unwrap()
            .get_array("$cond")
            .unwrap()[1],
        Bson::Document(doc! {"$literal": "car-1"})
    );
    assert!(set.contains_key("created_at"));
}

#[test]
fn mongo_update_and_delete_scope_owner_and_vehicle() {
    let (filter, update) = update_query("owner".into(), "car".into(), record());
    assert_eq!(filter, doc! { "_id": "owner", "vehicles.id": "car" });
    let set = update.get_document("$set").unwrap();
    assert!(set.contains_key("vehicles.$.vehicle"));
    assert!(!set.contains_key("vehicles.$.id"));
    assert!(!set.contains_key("vehicles.$.created_at"));
    assert!(!set.contains_key("active_vehicle_id"));
    let (filter, update) = delete_query("owner".into(), "$car".into());
    assert_eq!(filter, doc! { "_id": "owner", "vehicles.id": "$car" });
    assert_eq!(update.len(), 2);
    assert!(
        update[0]
            .get_document("$set")
            .unwrap()
            .contains_key("vehicles")
    );
    assert!(
        update[1]
            .get_document("$set")
            .unwrap()
            .contains_key("active_vehicle_id")
    );
}

#[test]
fn mongo_selection_query_guards_both_owner_and_contained_vehicle() {
    let (filter, update) = active_query("verified-user".into(), "car-1".into());
    assert_eq!(
        filter,
        doc! { "_id": "verified-user", "vehicles.id": "car-1" }
    );
    assert_eq!(
        update
            .get_document("$set")
            .unwrap()
            .get_str("active_vehicle_id")
            .unwrap(),
        "car-1"
    );
    assert!(!update.contains_key("$push"));
    assert!(!update.contains_key("$setOnInsert"));
}

#[test]
fn storage_rejects_malformed_records_and_empty_owners_before_writing() {
    for key in [
        "id",
        "name",
        "vehicle",
        "category",
        "verification",
        "created_at",
    ] {
        let mut invalid = record();
        invalid.remove(key);
        assert!(add_query("verified-user".into(), invalid).is_err(), "{key}");
    }
    for (key, value) in [
        ("verification", Bson::String("verified".into())),
        ("id", Bson::String(" ".into())),
        (
            "vehicle",
            Bson::Document(doc! { "password": "never-persist" }),
        ),
    ] {
        let mut invalid = record();
        invalid.insert(key, value);
        assert!(add_query("verified-user".into(), invalid).is_err());
    }
    let mut invalid = record();
    invalid.get_document_mut("vehicle").unwrap().insert(
        "future",
        doc! {
            "nested": [{"password": "never-persist"}],
        },
    );
    assert!(add_query("verified-user".into(), invalid).is_err());
    assert!(add_query(" ".into(), record()).is_err());
}

#[test]
fn raw_bson_read_preserves_literal_extended_json_keys() {
    let original = doc! { "vehicles": [record()] };
    let raw = mongodb::bson::RawDocumentBuf::from_document(&original).unwrap();
    assert_eq!(raw.to_document().unwrap(), original);
}

#[tokio::test]
#[ignore = "Opt in with GARAGE_TEST_MONGODB_URI; creates and drops only a unique test database"]
async fn mongo_adapter_end_to_end() {
    let uri = std::env::var("GARAGE_TEST_MONGODB_URI")
        .unwrap_or_else(|_| panic!("GARAGE_TEST_MONGODB_URI is required; URI is never printed"));
    let mut options = mongodb::options::ClientOptions::parse(uri)
        .await
        .unwrap_or_else(|_| panic!("Invalid MongoDB test URI; details suppressed"));
    options.server_selection_timeout = Some(std::time::Duration::from_secs(2));
    options.connect_timeout = Some(std::time::Duration::from_secs(2));
    let client = mongodb::Client::with_options(options)
        .unwrap_or_else(|_| panic!("Cannot construct MongoDB test client; details suppressed"));
    let suffix = mongodb::bson::oid::ObjectId::new().to_hex();
    let name = format!("brnavi_garage_test_{suffix}");
    assert_eq!(name.len(), "brnavi_garage_test_".len() + 24);
    assert!(name.starts_with("brnavi_garage_test_"));
    assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));
    let database = client.database(&name);
    assert!(
        database.run_command(doc! { "ping": 1 }).await.is_ok(),
        "MongoDB test server unavailable or unauthorized; details suppressed"
    );
    let exercise_database = database.clone();
    let result = tokio::spawn(async move {
        exercise_live_adapter(exercise_database).await;
    })
    .await;
    // Cleanup also runs after assertion panics in the exercise task.
    assert_eq!(database.name(), name);
    assert_eq!(database.name(), format!("brnavi_garage_test_{suffix}"));
    assert!(database.name().starts_with("brnavi_garage_test_"));
    let cleaned = database.drop().await.is_ok();
    assert!(
        cleaned,
        "Unique MongoDB test database cleanup failed; details suppressed"
    );
    assert!(
        result.is_ok(),
        "MongoDB adapter exercise failed (unique database cleaned)"
    );
    println!(
        "LIVE MongoDB adapter: two-owner isolation, persistence, snapshot, atomic selection, malformed data and storage failure checks passed; unique test database dropped."
    );
}

async fn exercise_live_adapter(database: Database) {
    let store = MongoGarage::new(database.clone());
    let first = record();
    let second = record();
    let other = record();
    let first_id = first.get_str("id").unwrap().to_owned();
    let second_id = second.get_str("id").unwrap().to_owned();
    let other_id = other.get_str("id").unwrap().to_owned();
    assert!(
        store
            .list("owner-a".into())
            .await
            .unwrap()
            .vehicles
            .is_empty()
    );
    assert!(
        store
            .list("owner-a".into())
            .await
            .unwrap()
            .active_vehicle_id
            .is_none()
    );
    assert!(store.add("owner-a".into(), first.clone()).await.is_ok());
    assert!(store.add("owner-a".into(), second.clone()).await.is_ok());
    assert!(store.add("owner-b".into(), other.clone()).await.is_ok());
    let a = store.list("owner-a".into()).await.unwrap();
    assert_eq!(a.vehicles, vec![first.clone(), second.clone()]);
    assert_eq!(a.active_vehicle_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(
        store.list("owner-b".into()).await.unwrap().vehicles,
        vec![other.clone()]
    );
    assert!(
        !store
            .set_active("owner-b".into(), first_id.clone())
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_active("owner-a".into(), other_id.clone())
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_active("owner-a".into(), "missing".into())
            .await
            .unwrap()
    );
    assert!(
        store
            .set_active("owner-a".into(), second_id.clone())
            .await
            .unwrap()
    );
    assert!(
        store
            .set_active("owner-a".into(), second_id.clone())
            .await
            .unwrap()
    );

    let reopened = MongoGarage::new(database.clone());
    let a = reopened.list("owner-a".into()).await.unwrap();
    assert_eq!(a.vehicles, vec![first, second]);
    assert_eq!(a.active_vehicle_id.as_deref(), Some(second_id.as_str()));
    let b = reopened.list("owner-b".into()).await.unwrap();
    assert_eq!(b.vehicles, vec![other]);
    assert_eq!(b.active_vehicle_id.as_deref(), Some(other_id.as_str()));
    let snapshot = super::super::bson_to_json(a.vehicles[0].get("vehicle").unwrap()).unwrap();
    assert_eq!(snapshot, crate::garage::tests::snapshot());

    let mut jobs = Vec::new();
    for _ in 0..8 {
        let adapter = reopened.clone();
        jobs.push(tokio::spawn(async move {
            adapter
                .add("concurrent-owner".into(), record())
                .await
                .is_ok()
        }));
    }
    for job in jobs {
        assert!(job.await.unwrap());
    }
    let concurrent = reopened.list("concurrent-owner".into()).await.unwrap();
    assert_eq!(concurrent.vehicles.len(), 8);
    assert_eq!(
        concurrent.active_vehicle_id.as_deref(),
        Some(concurrent.vehicles[0].get_str("id").unwrap())
    );
    let id1 = concurrent.vehicles[1].get_str("id").unwrap().to_owned();
    let id2 = concurrent.vehicles[2].get_str("id").unwrap().to_owned();
    let (left, right) = tokio::join!(
        reopened.set_active("concurrent-owner".into(), id1.clone()),
        reopened.set_active("concurrent-owner".into(), id2.clone()),
    );
    assert!(left.unwrap() && right.unwrap());
    let selected = reopened.list("concurrent-owner".into()).await.unwrap();
    assert!([Some(id1), Some(id2)].contains(&selected.active_vehicle_id));

    let mut replacement = record();
    replacement.insert("category", "commercial");
    let edited = reopened
        .update("owner-a".into(), first_id.clone(), replacement.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.get_str("id").unwrap(), first_id);
    assert_eq!(edited.get("created_at"), a.vehicles[0].get("created_at"));
    assert_eq!(edited.get_str("category").unwrap(), "commercial");
    assert!(
        reopened
            .update("owner-b".into(), first_id.clone(), replacement)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        reopened
            .delete("owner-b".into(), first_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened
            .delete("owner-a".into(), second_id.clone())
            .await
            .unwrap(),
        Some(Some(first_id.clone()))
    );
    assert_eq!(
        reopened
            .delete("owner-a".into(), first_id.clone())
            .await
            .unwrap(),
        Some(None)
    );
    assert_eq!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .active_vehicle_id,
        None
    );
    assert!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .vehicles
            .is_empty()
    );
    let replacement_first = record();
    let replacement_id = replacement_first.get_str("id").unwrap().to_owned();
    assert!(
        reopened
            .add("owner-a".into(), replacement_first)
            .await
            .is_ok()
    );
    assert_eq!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .active_vehicle_id,
        Some(replacement_id.clone())
    );
    assert!(reopened.add("owner-a".into(), record()).await.is_ok());

    // Both operations serialize on the same document, including delete-last vs add.
    for i in 0..10 {
        let owner = format!("delete-add-{i}");
        let old = record();
        let old_id = old.get_str("id").unwrap().to_owned();
        store.add(owner.clone(), old).await.unwrap();
        let new = record();
        let new_id = new.get_str("id").unwrap().to_owned();
        let (deleted, added) = tokio::join!(
            store.delete(owner.clone(), old_id),
            store.add(owner.clone(), new),
        );
        assert!(deleted.unwrap().is_some());
        added.unwrap();
        let state = store.list(owner).await.unwrap();
        assert_eq!(state.vehicles.len(), 1);
        assert_eq!(state.active_vehicle_id, Some(new_id));
    }
    for i in 0..10 {
        let owner = format!("update-delete-{i}");
        let old = record();
        let old_id = old.get_str("id").unwrap().to_owned();
        store.add(owner.clone(), old).await.unwrap();
        let (updated, deleted, selected) = tokio::join!(
            store.update(owner.clone(), old_id.clone(), record()),
            store.delete(owner.clone(), old_id.clone()),
            store.set_active(owner.clone(), old_id),
        );
        updated.unwrap();
        assert_eq!(deleted.unwrap(), Some(None));
        selected.unwrap();
        let state = store.list(owner).await.unwrap();
        assert!(state.vehicles.is_empty());
        assert_eq!(state.active_vehicle_id, None);
    }

    assert!(
        reopened
            .add("owner-a".into(), doc! { "id": "invalid" })
            .await
            .is_err()
    );
    assert_eq!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .vehicles
            .len(),
        2
    );
    let mut oversized = record();
    oversized
        .get_document_mut("vehicle")
        .unwrap()
        .insert("large", "x".repeat(17 * 1024 * 1024));
    assert!(
        reopened
            .add("owner-a".into(), oversized.clone())
            .await
            .is_err()
    );
    let before_oversized_update = reopened.list("owner-a".into()).await.unwrap().vehicles;
    assert!(
        reopened
            .update("owner-a".into(), replacement_id.clone(), oversized)
            .await
            .is_err()
    );
    assert_eq!(
        reopened.list("owner-a".into()).await.unwrap().vehicles,
        before_oversized_update
    );
    assert_eq!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .vehicles
            .len(),
        2
    );
    assert_eq!(
        reopened
            .list("owner-a".into())
            .await
            .unwrap()
            .active_vehicle_id
            .as_deref(),
        Some(replacement_id.as_str())
    );

    for (owner, vehicles, active) in [
        (
            "malformed-array",
            Bson::String("invalid".into()),
            "missing".to_owned(),
        ),
        (
            "malformed-record",
            Bson::Array(vec![Bson::Document(doc! { "id": "incomplete" })]),
            "incomplete".to_owned(),
        ),
        (
            "dangling-active",
            Bson::Array(vec![Bson::Document(record())]),
            "missing".to_owned(),
        ),
    ] {
        assert!(
            store
                .collection
                .insert_one(doc! {
                    "_id": owner, "vehicles": vehicles, "active_vehicle_id": active,
                })
                .await
                .is_ok()
        );
        assert!(reopened.list(owner.into()).await.is_err());
    }
}
