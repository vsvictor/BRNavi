use super::*;
use json_rpc_2_oauth2::{SqliteAuthServer, TokenConfig};
use pasetors::{
    keys::{Generate, SymmetricKey},
    paserk::FormatAsPaserk,
    version4::V4,
};

#[tokio::test]
async fn subscribe_requires_a_valid_token() {
    let mut key = String::new();
    SymmetricKey::<V4>::generate()
        .unwrap()
        .fmt(&mut key)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut storage = SqliteAuthServer::new(&key, TokenConfig::new("brnavi-test")).unwrap();
    storage.set_db_host(directory.path().join("auth.sqlite").to_str().unwrap());
    let auth = AuthPlugin::new(storage);
    let rpc = RpcServer::new();
    register_subscribe(&auth, &rpc, None).unwrap();
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "navi.callback.subscribe",
        "params": {"access_token": "invalid"},
    }))
    .unwrap();
    let reply: Value = serde_json::from_slice(&rpc.handle_bytes(&request).await.unwrap()).unwrap();
    assert_eq!(reply["error"]["code"], -32001);
}

#[tokio::test]
async fn notifier_without_control_plane_is_a_no_op() {
    let notifier = reset_notifier(None);
    notifier(PasswordResetNotice {
        email: "a@b.c".into(),
        reset_token: "secret".into(),
        user_id: "u1".into(),
    });
}
