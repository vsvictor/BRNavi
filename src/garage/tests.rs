use super::*;
use json_rpc_2_oauth2::{ServerPlugin, SqliteAuthServer, TokenConfig};
use pasetors::{
    keys::{Generate, SymmetricKey},
    paserk::FormatAsPaserk,
    version4::V4,
};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
struct FakeStore {
    state: Arc<Mutex<HashMap<String, GarageState>>>,
    fail: Arc<AtomicBool>,
}

impl FakeStore {
    fn check(&self) -> Result<(), RpcError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(RpcError::internal_error())
        } else {
            Ok(())
        }
    }
}

impl GarageStore for FakeStore {
    async fn add(&self, owner: String, vehicle: Document) -> Result<(), RpcError> {
        self.check()?;
        let mut states = self.state.lock().unwrap();
        let state = states.entry(owner).or_default();
        if state.active_vehicle_id.is_none() {
            state.active_vehicle_id = Some(vehicle.get_str("id").unwrap().to_owned());
        }
        state.vehicles.push(vehicle);
        Ok(())
    }

    async fn list(&self, owner: String) -> Result<GarageState, RpcError> {
        self.check()?;
        Ok(self
            .state
            .lock()
            .unwrap()
            .get(&owner)
            .map(|state| GarageState {
                vehicles: state.vehicles.clone(),
                active_vehicle_id: state.active_vehicle_id.clone(),
            })
            .unwrap_or_default())
    }

    async fn set_active(&self, owner: String, vehicle_id: String) -> Result<bool, RpcError> {
        self.check()?;
        let mut states = self.state.lock().unwrap();
        let Some(state) = states.get_mut(&owner) else {
            return Ok(false);
        };
        if !state
            .vehicles
            .iter()
            .any(|v| v.get_str("id").unwrap() == vehicle_id)
        {
            return Ok(false);
        }
        state.active_vehicle_id = Some(vehicle_id);
        Ok(true)
    }

    async fn update(
        &self,
        owner: String,
        vehicle_id: String,
        vehicle: Document,
    ) -> Result<Option<Document>, RpcError> {
        self.check()?;
        let mut states = self.state.lock().unwrap();
        let record = states.get_mut(&owner).and_then(|state| {
            state
                .vehicles
                .iter_mut()
                .find(|v| v.get_str("id") == Ok(vehicle_id.as_str()))
        });
        if let Some(record) = record {
            for key in ["name", "category", "vehicle"] {
                record.insert(key, vehicle.get(key).unwrap().clone());
            }
            Ok(Some(record.clone()))
        } else {
            Ok(None)
        }
    }

    async fn delete(
        &self,
        owner: String,
        vehicle_id: String,
    ) -> Result<Option<Option<String>>, RpcError> {
        self.check()?;
        let mut states = self.state.lock().unwrap();
        let Some(state) = states.get_mut(&owner) else {
            return Ok(None);
        };
        let Some(index) = state
            .vehicles
            .iter()
            .position(|v| v.get_str("id") == Ok(vehicle_id.as_str()))
        else {
            return Ok(None);
        };
        state.vehicles.remove(index);
        if state.active_vehicle_id.as_deref() == Some(&vehicle_id) {
            state.active_vehicle_id = state
                .vehicles
                .first()
                .map(|v| v.get_str("id").unwrap().to_owned());
        }
        Ok(Some(state.active_vehicle_id.clone()))
    }
}

async fn call(server: &RpcServer, method: &str, params: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .unwrap();
    serde_json::from_slice(&server.handle_bytes(&bytes).await.unwrap()).unwrap()
}

async fn setup() -> (tempfile::TempDir, RpcServer, FakeStore, Value, Value) {
    let directory = tempfile::Builder::new()
        .prefix(".garage-tests-")
        .tempdir_in(".")
        .unwrap();
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let mut storage = SqliteAuthServer::new(&key, TokenConfig::new("garage-tests")).unwrap();
    storage.set_db_host(directory.path().join("auth.sqlite").to_str().unwrap());
    let auth = AuthPlugin::new(storage);
    let server = RpcServer::new();
    auth.clone().install(&server).await.unwrap();
    let store = FakeStore::default();
    register_with_store(&auth, &server, store.clone()).unwrap();
    let mut tokens = Vec::new();
    for email in ["alice@example.test", "bob@example.test"] {
        let response = call(&server, "auth.register", json!({
            "email": email, "password": "garage-tests-password",
            "profile": { "first_name": "Garage", "last_name": "Test", "account_type": "private" },
        })).await;
        assert!(response.get("error").is_none());
        let response = call(
            &server,
            "auth.login",
            json!({
                "email": email, "password": "garage-tests-password", "grant_type": "password",
            }),
        )
        .await;
        assert!(response.get("error").is_none());
        tokens.push(response["result"]["access_token"].clone());
    }
    (directory, server, store, tokens.remove(0), tokens.remove(0))
}

pub(super) fn snapshot() -> Value {
    json!({
        "make": { "id": "17", "name": "Example", "raw_data": {"id": 17, "nested": {"x": true}} },
        "model": { "id": "27", "name": "Tourer", "raw_data": {"id": 27} },
        "year": 2024,
        "trim": { "id": "37", "name": "Sport", "raw_data": {"id": 37} },
        "bodies": [{"length": "123.40", "literal": {"$date": "not-a-date"}}],
        "engines": [{"displacement": 2.5}],
        "mileages": [],
        "exterior_colors": [{"name": "Blue", "rgb": [0, 50, 255]}],
        "interior_colors": [],
        "selected_exterior_color": {"name": "Blue", "rgb": [0, 50, 255]},
        "selected_interior_color": null,
        "vin": null,
        "vin_data": {"original": [false, null, 1.25]},
        "equipment_data": {"oem": {"features": ["heated seats"]}},
        "future_field": {"array": [1, "keep", null]},
    })
}

fn params(token: &Value) -> Value {
    json!({
        "access_token": token, "make_id": "17", "model_id": "27",
        "year": 2024, "trim_id": "37", "vehicle": snapshot(),
    })
}

#[tokio::test]
async fn vehicle_without_trim_can_be_added_listed_and_updated() {
    let (_directory, server, _store, alice, _) = setup().await;
    let mut input = params(&alice);
    input.as_object_mut().unwrap().remove("trim_id");
    input["vehicle"]["trim"] = Value::Null;
    let added = call(&server, "garage.vehicles.add", input.clone()).await;
    let vehicle = &added["result"]["vehicle"];
    assert!(vehicle["id"].is_string(), "{added}");
    assert!(vehicle["vehicle"]["trim"].is_null());
    let list = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(list["result"]["vehicles"][0]["id"], vehicle["id"]);
    input["vehicle_id"] = vehicle["id"].clone();
    let updated = call(&server, "garage.vehicles.update", input).await;
    assert_eq!(updated["result"]["vehicle"]["id"], vehicle["id"]);
    assert!(updated["result"]["vehicle"]["vehicle"]["trim"].is_null());
}

#[tokio::test]
async fn trim_id_and_snapshot_must_match_when_present() {
    let (_directory, server, _store, alice, _) = setup().await;
    let mut input = params(&alice);
    input["vehicle"]["trim"] = Value::Null;
    assert!(call(&server, "garage.vehicles.add", input.clone()).await["error"].is_object());
    input.as_object_mut().unwrap().remove("trim_id");
    input["vehicle"]["trim"] = snapshot()["trim"].clone();
    assert!(call(&server, "garage.vehicles.add", input).await["error"].is_object());
}

#[tokio::test]
async fn editing_and_deleting_are_owned_atomic_and_restore_first_active() {
    let (_directory, server, store, alice, bob) = setup().await;
    let first =
        call(&server, "garage.vehicles.add", params(&alice)).await["result"]["vehicle"].clone();
    let second =
        call(&server, "garage.vehicles.add", params(&alice)).await["result"]["vehicle"].clone();
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();
    let before = store
        .state
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .vehicles[0]
        .get("created_at")
        .unwrap()
        .clone();

    let mut update = params(&alice);
    update["vehicle_id"] = first["id"].clone();
    update["class_id"] = json!("commercial");
    update["vehicle"]["model"]["name"] = json!("Updated");
    update["vehicle"]["equipment_data"] = json!({"new": true});
    let changed = call(&server, "garage.vehicles.update", update.clone()).await;
    assert_eq!(changed["result"]["vehicle"]["id"], first_id);
    assert_eq!(
        changed["result"]["vehicle"]["name"],
        "Example Updated · 2024"
    );
    assert_eq!(changed["result"]["vehicle"]["category"], "commercial");
    assert_eq!(changed["result"]["vehicle"]["vehicle"], update["vehicle"]);
    assert_eq!(
        store
            .state
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .vehicles[0]
            .get("created_at"),
        Some(&before)
    );

    let mut foreign_update = update.clone();
    foreign_update["access_token"] = bob.clone();
    let mut missing_update = update.clone();
    missing_update["vehicle_id"] = json!("missing");
    for input in [foreign_update, missing_update] {
        assert_eq!(
            call(&server, "garage.vehicles.update", input).await["error"]["code"],
            -32602
        );
    }
    let mut invalid = update.clone();
    invalid["vehicle"]["make"]["id"] = json!("wrong");
    assert_eq!(
        call(&server, "garage.vehicles.update", invalid).await["error"]["code"],
        -32602
    );
    let mut unknown = update.clone();
    unknown["unexpected"] = json!(true);
    assert_eq!(
        call(&server, "garage.vehicles.update", unknown).await["error"]["code"],
        -32602
    );
    for id in [first_id, "missing"] {
        assert_eq!(
            call(
                &server,
                "garage.vehicles.delete",
                json!({"access_token": bob, "vehicle_id": id})
            )
            .await["error"]["code"],
            -32602
        );
    }
    assert_eq!(
        call(
            &server,
            "garage.vehicles.list",
            json!({"access_token": alice})
        )
        .await["result"]["vehicles"][0],
        changed["result"]["vehicle"]
    );
    assert_eq!(
        call(
            &server,
            "garage.vehicles.delete",
            json!({"access_token": alice, "vehicle_id": second_id})
        )
        .await["result"],
        json!({"active_vehicle_id": first_id})
    );
    assert_eq!(
        call(
            &server,
            "garage.vehicles.delete",
            json!({"access_token": alice, "vehicle_id": first_id})
        )
        .await["result"],
        json!({"active_vehicle_id": null})
    );
    assert_eq!(
        call(
            &server,
            "garage.vehicles.list",
            json!({"access_token": alice})
        )
        .await["result"],
        json!({"vehicles": [], "active_vehicle_id": null})
    );
    let next = call(&server, "garage.vehicles.add", params(&alice)).await;
    assert_eq!(
        call(
            &server,
            "garage.vehicles.list",
            json!({"access_token": alice})
        )
        .await["result"]["active_vehicle_id"],
        next["result"]["vehicle"]["id"]
    );
}

#[tokio::test]
async fn authenticated_users_are_isolated_and_snapshots_round_trip() {
    let (_directory, server, store, alice, bob) = setup().await;
    let empty = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(
        empty["result"],
        json!({"vehicles": [], "active_vehicle_id": null})
    );
    let first = call(&server, "garage.vehicles.add", params(&alice)).await;
    let second = call(&server, "garage.vehicles.add", params(&alice)).await;
    let third = call(&server, "garage.vehicles.add", params(&bob)).await;
    let first = &first["result"]["vehicle"];
    let second = &second["result"]["vehicle"];
    let third = &third["result"]["vehicle"];
    assert!(first["id"].is_string());
    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["name"], "Example Tourer · 2024");
    assert_eq!(first["category"], "passenger");
    assert_eq!(first["verification"], "none");
    assert_eq!(first["vehicle"], snapshot());
    let listed = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(
        listed["result"],
        json!({"vehicles": [first, second], "active_vehicle_id": first["id"]})
    );
    let foreign = call(
        &server,
        "garage.vehicles.set_active",
        json!({
            "access_token": bob, "vehicle_id": second["id"],
        }),
    )
    .await;
    assert_eq!(foreign["error"]["code"], -32602);
    let selected = call(
        &server,
        "garage.vehicles.set_active",
        json!({
            "access_token": alice, "vehicle_id": second["id"],
        }),
    )
    .await;
    assert_eq!(
        selected["result"],
        json!({"active_vehicle_id": second["id"]})
    );
    let selected_again = call(
        &server,
        "garage.vehicles.set_active",
        json!({
            "access_token": alice, "vehicle_id": second["id"],
        }),
    )
    .await;
    assert_eq!(selected_again["result"], selected["result"]);
    let bob_list = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": bob}),
    )
    .await;
    assert_eq!(
        bob_list["result"],
        json!({"vehicles": [third], "active_vehicle_id": third["id"]})
    );
    let alice_list = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(alice_list["result"]["active_vehicle_id"], second["id"]);
    let states = store.state.lock().unwrap();
    assert_eq!(states.len(), 2);
    for state in states.values() {
        for record in &state.vehicles {
            assert!(matches!(record.get("vehicle"), Some(Bson::Document(_))));
            assert!(matches!(record.get("created_at"), Some(Bson::DateTime(_))));
            assert!(!record.to_string().contains(alice.as_str().unwrap()));
            assert!(!record.to_string().contains(bob.as_str().unwrap()));
        }
    }
}

#[tokio::test]
async fn all_methods_require_auth_and_reject_owner_injection() {
    let (_directory, server, store, alice, _) = setup().await;
    for method in [
        "garage.vehicles.add",
        "garage.vehicles.list",
        "garage.vehicles.set_active",
        "garage.vehicles.update",
        "garage.vehicles.delete",
    ] {
        for credential in [json!({}), json!({"access_token": "invalid"})] {
            assert_eq!(
                call(&server, method, credential).await["error"]["code"],
                -32001
            );
        }
        let mut input = if method.ends_with("add") || method.ends_with("update") {
            params(&alice)
        } else {
            json!({"access_token": alice})
        };
        if method.ends_with("set_active")
            || method.ends_with("update")
            || method.ends_with("delete")
        {
            input["vehicle_id"] = json!("foreign-id");
        }
        for key in [
            "owner",
            "user_id",
            "verification",
            "created_at",
            "vehicle_id_injected",
        ] {
            let mut injected = input.clone();
            injected[key] = json!("bob");
            assert_eq!(
                call(&server, method, injected).await["error"]["code"],
                -32602
            );
        }
    }
    assert!(store.state.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_snapshots_and_nested_credentials_are_rejected() {
    let (_directory, server, store, alice, _) = setup().await;
    for (pointer, value) in [
        ("/vehicle", json!([])),
        ("/year", json!(0)),
        ("/year", json!(2023)),
        ("/year", json!(2024.5)),
        ("/make_id", json!("mismatch")),
        ("/vehicle/make/id", json!("")),
        ("/vehicle/model/name", json!("  ")),
        ("/vehicle/trim/id", json!(37)),
        ("/vehicle/trim/raw_data", json!([])),
        ("/vehicle/bodies", json!({})),
        ("/vehicle/engines", json!([null])),
        ("/vehicle/selected_exterior_color", json!("Blue")),
        ("/vehicle/selected_interior_color", json!([])),
        ("/vehicle/vin_data", json!("invalid")),
        ("/vehicle/vin", json!(123)),
        ("/vehicle/equipment_data", json!([])),
        (
            "/vehicle/future_field",
            json!({"nested": [{"access_token": "do-not-store"}]}),
        ),
        (
            "/vehicle/future_field",
            json!({"Authorization": "do-not-store"}),
        ),
    ] {
        let mut input = params(&alice);
        *input.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            call(&server, "garage.vehicles.add", input).await["error"]["code"],
            -32602,
            "{pointer}"
        );
    }

    assert!(store.state.lock().unwrap().is_empty());
    let mut valid = params(&alice);
    valid["class_id"] = json!("legacy-c");
    for key in [
        "bodies",
        "engines",
        "mileages",
        "exterior_colors",
        "interior_colors",
    ] {
        valid["vehicle"][key] = json!([]);
    }
    assert_eq!(
        call(&server, "garage.vehicles.add", valid).await["result"]["vehicle"]["category"],
        "legacy-c"
    );
}

#[tokio::test]
async fn recursive_credentials_and_snapshot_owner_fields_cannot_change_ownership() {
    let (_directory, server, store, alice, bob) = setup().await;
    for key in [
        "access_token",
        "refresh_token",
        "password",
        "PaSsWoRd",
        "ACCESS_TOKEN",
    ] {
        for section in ["future_field", "equipment_data", "make"] {
            let mut input = params(&alice);
            input["vehicle"][section]["unknown"] = json!({"nested": [{"deeper": {key: "secret"}}]});
            assert_eq!(
                call(&server, "garage.vehicles.add", input).await["error"]["code"],
                -32602
            );
        }
    }
    assert!(store.state.lock().unwrap().is_empty());
    let mut input = params(&alice);
    input["vehicle"]["owner"] = json!("not-the-authenticated-user");
    input["vehicle"]["user_id"] = json!("not-the-authenticated-user");
    input["vehicle"]["verification"] = json!("verified");
    let saved = call(&server, "garage.vehicles.add", input.clone()).await;
    assert_eq!(saved["result"]["vehicle"]["verification"], "none");
    assert_eq!(saved["result"]["vehicle"]["vehicle"], input["vehicle"]);
    let bob_list = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": bob}),
    )
    .await;
    assert_eq!(bob_list["result"]["vehicles"], json!([]));
    let alice_list = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(
        alice_list["result"]["vehicles"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn concurrent_add_and_selection_keep_one_owned_active_vehicle() {
    let (_directory, server, _, alice, _) = setup().await;
    let (left, right) = tokio::join!(
        call(&server, "garage.vehicles.add", params(&alice)),
        call(&server, "garage.vehicles.add", params(&alice)),
    );
    let left_id = &left["result"]["vehicle"]["id"];
    let right_id = &right["result"]["vehicle"]["id"];
    let listed = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(listed["result"]["vehicles"].as_array().unwrap().len(), 2);
    assert!([left_id, right_id].contains(&&listed["result"]["active_vehicle_id"]));
    let (left, right) = tokio::join!(
        call(
            &server,
            "garage.vehicles.set_active",
            json!({"access_token": alice, "vehicle_id": left_id})
        ),
        call(
            &server,
            "garage.vehicles.set_active",
            json!({"access_token": alice, "vehicle_id": right_id})
        ),
    );
    assert!(left.get("error").is_none() && right.get("error").is_none());
    let listed = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert!([left_id, right_id].contains(&&listed["result"]["active_vehicle_id"]));
}

#[tokio::test]
async fn storage_failures_are_internal_errors_without_partial_fake_writes() {
    let (_directory, server, store, alice, _) = setup().await;
    let saved = call(&server, "garage.vehicles.add", params(&alice)).await;
    store.fail.store(true, Ordering::SeqCst);
    for (method, input) in [
        ("garage.vehicles.add", params(&alice)),
        ("garage.vehicles.list", json!({"access_token": alice})),
        (
            "garage.vehicles.set_active",
            json!({
                "access_token": alice, "vehicle_id": saved["result"]["vehicle"]["id"],
            }),
        ),
        ("garage.vehicles.update", {
            let mut input = params(&alice);
            input["vehicle_id"] = saved["result"]["vehicle"]["id"].clone();
            input
        }),
        (
            "garage.vehicles.delete",
            json!({
                "access_token": alice, "vehicle_id": saved["result"]["vehicle"]["id"],
            }),
        ),
    ] {
        assert_eq!(call(&server, method, input).await["error"]["code"], -32603);
    }
    store.fail.store(false, Ordering::SeqCst);
    let listed = call(
        &server,
        "garage.vehicles.list",
        json!({"access_token": alice}),
    )
    .await;
    assert_eq!(listed["result"]["vehicles"].as_array().unwrap().len(), 1);
}

#[test]
fn bson_preserves_literal_extended_json_and_rejects_unrepresentable_numbers() {
    let raw = json!({"$date": "literal", "$numberLong": "2", "fraction": 1.5, "nested": [{"$oid": "raw"}]});
    assert_eq!(
        bson_to_json(&json_to_bson(raw.clone()).unwrap()).unwrap(),
        raw
    );
    assert!(json_to_bson(json!(u64::MAX)).is_err());
}
