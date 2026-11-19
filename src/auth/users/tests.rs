use super::{
    accounts::CreateUserRequest,
    profile::{parse_profile, user_json},
    register_user_admin_methods,
    roles::{parse_role_name, parse_role_permissions},
};
use json_rpc_2_oauth2::{AuthPlugin, MongoAuthServer, RpcServer, TokenConfig};
use mongodb::{
    Client,
    bson::{Bson, DateTime, doc, oid::ObjectId},
};
use pasetors::{
    keys::{Generate, SymmetricKey},
    paserk::FormatAsPaserk,
    version4::V4,
};
use serde_json::{Value, json};
use std::collections::HashMap;

#[test]
fn profile_values_are_converted_according_to_the_registered_schema() {
    let profile = parse_profile(HashMap::from([
        ("first_name".into(), "Ada".into()),
        ("birth_date".into(), "2000-01-02T03:04:05Z".into()),
        ("notifications_enabled".into(), "true".into()),
    ]))
    .unwrap();
    assert_eq!(profile.get_str("first_name").unwrap(), "Ada");
    assert!(matches!(profile.get("birth_date"), Some(Bson::DateTime(_))));
    assert_eq!(profile.get_bool("notifications_enabled").unwrap(), true);
    assert!(parse_profile(HashMap::from([("unknown".into(), "x".into())])).is_err());
}

#[test]
fn user_responses_do_not_include_password_hashes() {
    let document = doc! {
        "_id": ObjectId::new(),
        "email": "user@example.test",
        "password_hash": "never-return-this",
        "created_at": DateTime::now(),
        "roles": ["user"],
        "profile": { "first_name": "Ada" },
    };
    let response = user_json(document).unwrap();
    let encoded = response.to_string();
    assert!(encoded.contains("user@example.test"));
    assert!(!encoded.contains("never-return-this"));
}

#[test]
fn administrator_contacts_use_shared_validation_and_preserve_login_email() {
    let contacts = json!([
        {"type":"email","value":"contact@example.invalid"},
        {"type":"telegram","value":"@example_contact"}
    ]);
    let request: CreateUserRequest = serde_json::from_value(json!({
        "email":"login@example.invalid","password":"test-only-password",
        "profile":{"first_name":"Example","contacts":contacts}
    }))
    .unwrap();
    let profile = parse_profile(request.profile.unwrap()).unwrap();
    assert!(matches!(profile.get("contacts"), Some(Bson::Array(_))));
    let document = doc! {
        "_id": ObjectId::new(), "email": request.email,
        "created_at": DateTime::now(), "roles": [], "profile": profile,
    };
    let response = user_json(document.clone()).unwrap();
    assert_eq!(response["email"], "login@example.invalid");
    assert_eq!(response["profile"]["contacts"], contacts);
    for value in [
        json!([{"type":"unknown","value":"x"}]),
        json!([{"type":"email","value":"invalid"}]),
        json!([{"type":"phone","value":"123"}]),
        json!(null),
    ] {
        assert!(parse_profile(HashMap::from([("contacts".into(), value)])).is_err());
    }
    let mut empty = document.clone();
    empty.insert("profile", doc! {"contacts": []});
    assert_eq!(user_json(empty).unwrap()["profile"]["contacts"], json!([]));
    let mut legacy = document;
    legacy.insert("profile", doc! {"first_name":"Example"});
    assert_eq!(
        user_json(legacy).unwrap()["profile"]["contacts"],
        json!([{"type":"email","value":"login@example.invalid"}])
    );
    let legacy_input = parse_profile(HashMap::from([("contacts".into(), json!("[]"))])).unwrap();
    assert_eq!(legacy_input.get_array("contacts").unwrap().len(), 0);
    assert!(parse_profile(HashMap::from([("first_name".into(), json!(true))])).is_err());
}

#[test]
fn role_names_and_permissions_are_validated() {
    assert_eq!(
        parse_role_name(" support-viewer ").unwrap(),
        "support-viewer"
    );
    assert!(parse_role_name("").is_err());
    assert!(parse_role_name("system.internal").is_err());
    assert_eq!(
        parse_role_permissions(vec!["rbac.users.read".into(), " rbac.users.read ".into(),])
            .unwrap(),
        ["rbac.users.read"]
    );
    assert!(parse_role_permissions(vec![]).is_err());
    assert!(parse_role_permissions(vec!["*".into()]).is_err());
}

#[tokio::test]
async fn admin_user_list_requires_a_users_read_permission() {
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let auth =
        AuthPlugin::new(MongoAuthServer::new(&key, TokenConfig::new("brnavi-test")).unwrap());
    let client = Client::with_uri_str("mongodb://127.0.0.1:27017")
        .await
        .unwrap();
    let rpc = RpcServer::new();
    register_user_admin_methods(&auth, &rpc, client.database("brnavi-test")).unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "admin.users.list",
        "params": {}
    }))
    .unwrap();
    let response: Value =
        serde_json::from_slice(&rpc.handle_bytes(&request).await.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32001);
}

#[tokio::test]
async fn admin_role_list_requires_role_management_permission() {
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let auth =
        AuthPlugin::new(MongoAuthServer::new(&key, TokenConfig::new("brnavi-test")).unwrap());
    let client = Client::with_uri_str("mongodb://127.0.0.1:27017")
        .await
        .unwrap();
    let rpc = RpcServer::new();
    register_user_admin_methods(&auth, &rpc, client.database("brnavi-test")).unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "admin.roles.list",
        "params": {}
    }))
    .unwrap();
    let response: Value =
        serde_json::from_slice(&rpc.handle_bytes(&request).await.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32001);
}

#[tokio::test]
async fn admin_role_create_requires_role_management_permission() {
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let auth =
        AuthPlugin::new(MongoAuthServer::new(&key, TokenConfig::new("brnavi-test")).unwrap());
    let client = Client::with_uri_str("mongodb://127.0.0.1:27017")
        .await
        .unwrap();
    let rpc = RpcServer::new();
    register_user_admin_methods(&auth, &rpc, client.database("brnavi-test")).unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "admin.roles.create",
        "params": {
            "name": "support-viewer",
            "permissions": ["rbac.users.read"]
        }
    }))
    .unwrap();
    let response: Value =
        serde_json::from_slice(&rpc.handle_bytes(&request).await.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32001);
}

#[tokio::test]
async fn admin_role_update_requires_role_management_permission() {
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let auth =
        AuthPlugin::new(MongoAuthServer::new(&key, TokenConfig::new("brnavi-test")).unwrap());
    let client = Client::with_uri_str("mongodb://127.0.0.1:27017")
        .await
        .unwrap();
    let rpc = RpcServer::new();
    register_user_admin_methods(&auth, &rpc, client.database("brnavi-test")).unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "admin.roles.update",
        "params": {
            "name": "support-viewer",
            "permissions": ["rbac.users.read"]
        }
    }))
    .unwrap();
    let response: Value =
        serde_json::from_slice(&rpc.handle_bytes(&request).await.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32001);
}
