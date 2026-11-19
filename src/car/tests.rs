use super::*;
use json_rpc_2_oauth2::{SqliteAuthServer, TokenConfig};
use pasetors::{
    keys::{Generate, SymmetricKey},
    paserk::FormatAsPaserk,
    version4::V4,
};

async fn call(server: &RpcServer, method: &str, params: Value) -> Value {
    let input = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 42, "method": method, "params": params,
    }))
    .unwrap();
    let output = server.handle_bytes(&input).await.unwrap();
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["id"], 42);
    response
}

async fn setup() -> (
    tempfile::TempDir,
    AuthPlugin<SqliteAuthServer>,
    RpcServer,
    Value,
    Value,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let mut storage = SqliteAuthServer::new(&key, TokenConfig::new("car-test")).unwrap();
    storage.set_db_host(directory.path().join("auth.sqlite").to_str().unwrap());
    let auth = AuthPlugin::new(storage);
    let server = RpcServer::new();
    auth.clone().install(&server).await.unwrap();
    let registered = call(
        &server,
        "auth.register",
        json!({
            "email": "car@example.test", "password": "car-integration-password",
            "profile": {"first_name": "Car", "last_name": "User", "account_type": "private"},
        }),
    )
    .await;
    assert!(registered.get("error").is_none(), "{registered}");
    let login = call(
        &server,
        "auth.login",
        json!({"email": "car@example.test", "password": "car-integration-password", "grant_type": "password"}),
    ).await;
    assert!(login.get("error").is_none(), "{login}");
    (
        directory,
        auth,
        server,
        login["result"]["access_token"].clone(),
        login["result"]["refresh_token"].clone(),
    )
}

#[tokio::test]
async fn authorized_dispatch_strips_credentials_and_preserves_results_and_errors() {
    let (_directory, auth, server, token, refresh) = setup().await;
    let upstream = RpcServer::new();
    upstream
        .register("car.vindecoder_index_get", |params| async move {
            assert_eq!(
                params,
                json!({
                    "vin": "1GTG6CEN0L1139305", "verbose": "yes", "all_trims": "yes",
                })
            );
            Ok(json!({"specs": {"make": "GMC"}, "trims": []}))
        })
        .unwrap();
    upstream
        .register("car.failure", |_| async {
            Err(RpcError::new(-32000, "CarAPI returned HTTP 429").with_data(json!({"retry": true})))
        })
        .unwrap();
    upstream
        .register("car.null", |_| async { Ok(Value::Null) })
        .unwrap();
    register_authorized(&auth, &server, upstream).unwrap();
    for credential in [
        json!({"access_token": token}),
        json!({"authorization": format!("Bearer {}", token.as_str().unwrap())}),
    ] {
        let mut params = json!({
            "vin": "1GTG6CEN0L1139305", "verbose": "yes", "all_trims": "yes",
        });
        params
            .as_object_mut()
            .unwrap()
            .extend(credential.as_object().unwrap().clone());
        let response = call(&server, "car.vindecoder_index_get", params).await;
        assert_eq!(
            response["result"],
            json!({"specs": {"make": "GMC"}, "trims": []})
        );
    }
    for credential in [Value::Null, json!("invalid"), refresh] {
        let response = call(
            &server,
            "car.vindecoder_index_get",
            json!({
                "vin": "1GTG6CEN0L1139305", "access_token": credential,
            }),
        )
        .await;
        assert_eq!(response["error"]["code"], -32001);
    }
    let error = call(&server, "car.failure", json!({"access_token": token})).await;
    assert_eq!(
        error["error"],
        json!({
            "code": -32000, "message": "CarAPI returned HTTP 429", "data": {"retry": true},
        })
    );
    let null = call(&server, "car.null", json!({"access_token": token})).await;
    assert!(null.get("error").is_none(), "{null}");
    assert_eq!(null.get("result"), Some(&Value::Null));
}

#[tokio::test]
async fn catalog_methods_adapt_authenticated_car_api_results() {
    let (_directory, auth, server, token, _) = setup().await;
    let upstream = RpcServer::new();
    upstream
        .register("car.makes_index2_get", |params| async move {
            assert_eq!(params["page"], 1);
            assert_eq!(params["limit"], 100);
            Ok(json!({
                "data": [{"id": 42, "name": "Toyota"}],
                "collection": {"pages": 1},
            }))
        })
        .unwrap();
    upstream
        .register("car.oemmakemodels_index_get", |params| async move {
            assert_eq!(params["page"], 1);
            assert_eq!(params["limit"], 100);
            assert_eq!(params["make_id"], "42");
            Ok(json!({
                "data": [{"id": 7, "name": "Camry"}],
                "collection": {"pages": 1},
            }))
        })
        .unwrap();
    register_authorized(&auth, &server, upstream).unwrap();

    let classes = call(
        &server,
        "catalog.classes.list",
        json!({"access_token": token}),
    )
    .await;
    assert_eq!(
        classes["result"]["classes"],
        json!([
            {"id": "passenger", "name": "Легкові"},
            {"id": "truck", "name": "Вантажні"},
        ])
    );

    let makes = call(
        &server,
        "catalog.makes.list",
        json!({"access_token": token, "class_id": "passenger"}),
    )
    .await;
    assert_eq!(
        makes["result"]["makes"],
        json!([{"id": "42", "name": "Toyota"}])
    );

    let models = call(
        &server,
        "catalog.models.list",
        json!({"access_token": token, "make_id": "42"}),
    )
    .await;
    assert_eq!(
        models["result"]["models"],
        json!([{"id": "7", "name": "Camry", "years": [], "trims": []}])
    );

    assert_eq!(
        call(&server, "catalog.classes.list", json!({})).await["error"]["code"],
        -32001
    );
    assert_eq!(
        call(
            &server,
            "catalog.makes.list",
            json!({"access_token": token, "class_id": "unknown"}),
        )
        .await["error"]["code"],
        -32602
    );
}

#[tokio::test]
async fn installed_car_plugin_requires_auth_and_keeps_business_validation() {
    let (_directory, auth, server, token, _) = setup().await;
    let config = CarConfig {
        greeting: "Hello".into(),
        reset_secs: std::time::Duration::from_secs(3600),
        carapi_api_token: String::new(),
        carapi_api_secret: String::new(),
    };
    let handle = AuthorizedCar::new(config, auth)
        .install(&server)
        .await
        .unwrap();
    assert_eq!(handle.maintenance().len(), 1);
    let methods: Vec<_> = server
        .method_names()
        .into_iter()
        .filter(|name| name.starts_with("car."))
        .collect();
    assert!(
        methods
            .iter()
            .any(|name| name == "car.vindecoder_index_get")
    );
    for method in methods {
        for params in [json!({}), json!({"access_token": "invalid"})] {
            assert_eq!(
                call(&server, &method, params).await["error"]["code"],
                -32001,
                "{method}"
            );
        }
    }
    for params in [
        json!({"access_token": token, "verbose": "yes", "all_trims": "yes"}),
        json!({"access_token": token, "vin": 123}),
        json!({"access_token": token, "vin": "1GTG6CEN0L1139305", "unexpected": true}),
    ] {
        assert_eq!(
            call(&server, "car.vindecoder_index_get", params).await["error"]["code"],
            -32602
        );
    }
}
