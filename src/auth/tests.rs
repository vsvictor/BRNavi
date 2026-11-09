use super::*;
use json_rpc_2_oauth2::{ServerPlugin, SqliteAuthServer};
use pasetors::{
    keys::{Generate, SymmetricKey},
    paserk::FormatAsPaserk,
    version4::V4,
};

async fn call(rpc: &RpcServer, method: &str, params: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .unwrap();
    serde_json::from_slice(&rpc.handle_bytes(&bytes).await.unwrap()).unwrap()
}

async fn verify_shared_auth<S>(storage: impl Fn(&str) -> S)
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let first = RpcServer::new();
    let second = RpcServer::new();
    for rpc in [&first, &second] {
        let auth = AuthPlugin::new(storage(&key));
        register(&auth, rpc).unwrap();
        auth.install(rpc).await.unwrap();
    }
    for params in [json!({}), json!({"access_token": "invalid"})] {
        assert_eq!(
            call(&second, "navi.me", params).await["error"]["code"],
            -32001
        );
    }
    let user = call(
        &first,
        "auth.register",
        json!({"email": "mobile@example.test", "password": "integration-test-password", "profile": {"first_name": "Mobile", "last_name": "User", "account_type": "private"}}),
    )
    .await;
    assert!(user.get("error").is_none(), "{user}");
    let user_id = user["result"]["id"].clone();
    let login = call(
        &second,
        "auth.login",
        json!({"email": "mobile@example.test", "password": "integration-test-password", "grant_type": "password"}),
    )
    .await;
    assert!(login.get("error").is_none(), "{login}");
    let token = &login["result"]["access_token"];
    for rpc in [&first, &second] {
        let profile = call(
            rpc,
            "auth.getProfile",
            json!({
                "access_token": token, "user_id": "forged",
            }),
        )
        .await;
        assert_eq!(profile["result"]["id"], user_id);
        assert_eq!(profile["result"]["email"], "mobile@example.test");
        assert_eq!(
            profile["result"]["profile"],
            json!({"first_name": "Mobile", "last_name": "User", "account_type": "private"})
        );
        assert!(profile["result"].get("password_hash").is_none());
        assert_eq!(
            call(rpc, "auth.getProfile", json!({})).await["error"]["code"],
            -32001
        );
    }
    for rpc in [&first, &second] {
        let me = call(
            rpc,
            "navi.me",
            json!({"access_token": token, "user_id": "forged", "roles": ["admin"]}),
        )
        .await;
        assert_eq!(me["result"]["user_id"], user_id);
        assert_eq!(me["result"]["roles"], json!([]));
        assert_eq!(me["result"]["permissions"], json!([]));
        assert!(me["result"].get("access_token").is_none());
    }
    let me = call(
        &first,
        "navi.me",
        json!({"authorization": format!("Bearer {}", token.as_str().unwrap())}),
    )
    .await;
    assert_eq!(me["result"]["user_id"], user_id);
    assert_eq!(
        call(
            &first,
            "navi.me",
            json!({"access_token": login["result"]["refresh_token"]})
        )
        .await["error"]["code"],
        -32001
    );
    assert_eq!(
        call(
            &second,
            "rbac.addRoleToUser",
            json!({
                "access_token": token, "user_id": user_id, "role_name": "admin",
            })
        )
        .await["error"]["code"],
        -32003
    );
    let refreshed = call(
        &first,
        "auth.refresh",
        json!({"refresh_token": login["result"]["refresh_token"]}),
    )
    .await;
    assert!(refreshed.get("error").is_none(), "{refreshed}");
    assert_eq!(
        call(
            &second,
            "navi.me",
            json!({"access_token": refreshed["result"]["access_token"]})
        )
        .await["result"]["user_id"],
        user_id
    );
}

#[tokio::test]
async fn annotations_and_auth_rpc_share_users_and_tokens_between_instances() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.sqlite");
    verify_shared_auth(|key| {
        let mut storage = SqliteAuthServer::new(key, TokenConfig::new("brnavi-test")).unwrap();
        storage.set_db_host(path.to_str().unwrap());
        storage
    })
    .await;
}

#[tokio::test]
#[ignore = "Requires MongoDB in BRNAVI_TEST_DATABASE_URL"]
async fn mongodb_auth_across_instances() {
    let uri =
        std::env::var("BRNAVI_TEST_DATABASE_URL").expect("BRNAVI_TEST_DATABASE_URL is required");
    let database = std::env::var("BRNAVI_TEST_DATABASE_NAME").unwrap_or_else(|_| {
        format!(
            "brnavi_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    });
    verify_shared_auth(|key| {
        let mut storage = MongoAuthServer::new(key, TokenConfig::new("brnavi-test")).unwrap();
        storage.set_db_host(&uri);
        storage.set_db_name(&database);
        storage
    })
    .await;
}
