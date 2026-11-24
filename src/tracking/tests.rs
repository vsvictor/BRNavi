use super::storage::{
    grant_from_document, grant_key, point_document, point_key, stored_point, tracking_role_is_safe,
    tracking_role_write,
};
use super::*;
use json_rpc_2_coopnav::{CoopNavPlugin, InMemoryCoopNavStorage, StoragePartition, TrackPoint};
use json_rpc_2_oauth2::{
    JsonRpcAuthServer, LoginRequest, RegisterRequest, SqliteAuthServer, TokenConfig,
};
use json_rpc_2_server::{JsonRpcProtocol, TransportContext};
use mongodb::bson::doc;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};

const TEST_KEY: &str = "k4.local.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const DEVICE: &str = "install-1234abcd";
const NOW: i64 = 1_763_500_000_000;

#[derive(Default)]
struct FakeSessions(Mutex<HashMap<(String, String), DeviceSession>>);

impl FakeSessions {
    fn login(&self, user: &str, device: &str, sid: &str, last_seen_ms: i64) {
        self.0.lock().unwrap().insert(
            (user.to_owned(), device.to_owned()),
            DeviceSession {
                sid: sid.to_owned(),
                last_seen_ms,
            },
        );
    }
}

#[async_trait]
impl DeviceSessions for FakeSessions {
    async fn current(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<DeviceSession>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(&(user_id.to_owned(), device_id.to_owned()))
            .cloned())
    }
}

/// Mirrors `MongoTrackingGrants`: one grant per `(subject, device, auth_sid)`.
#[derive(Default)]
struct FakeGrants(Mutex<Vec<TrackingGrant>>);

#[async_trait]
impl TrackingGrants for FakeGrants {
    async fn open(&self, candidate: TrackingGrant) -> Result<TrackingGrant, StoreError> {
        let mut grants = self.0.lock().unwrap();
        if let Some(existing) = grants.iter().find(|g| {
            g.subject == candidate.subject
                && g.device_id == candidate.device_id
                && g.auth_sid == candidate.auth_sid
        }) {
            return Ok(existing.clone());
        }
        grants.retain(|g| g.subject != candidate.subject || g.device_id != candidate.device_id);
        grants.push(candidate.clone());
        Ok(candidate)
    }

    async fn find(&self, session_id: &str) -> Result<Option<TrackingGrant>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|g| g.session_id == session_id)
            .cloned())
    }
}

struct Harness {
    server: RpcServer,
    sessions: Arc<FakeSessions>,
    grants: Arc<FakeGrants>,
    storage: Arc<InMemoryCoopNavStorage>,
    now: Arc<AtomicI64>,
    users: Vec<(String, String)>,
    refresh_tokens: Vec<String>,
    identity: SqliteAuthServer,
    permissions: Arc<TestPermissions>,
}

struct TestPermissions {
    identity: SqliteAuthServer,
    subjects: Mutex<Vec<String>>,
    fail: AtomicBool,
}

#[async_trait]
impl TrackingPermissions for TestPermissions {
    async fn ensure_owner(&self, subject: &str) -> Result<(), StoreError> {
        self.subjects.lock().unwrap().push(subject.to_owned());
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError);
        }
        self.identity
            .handle_add_role(subject.to_owned(), TRACKING_ROLE.into())
            .await
            .map_err(|_| StoreError)
    }
}

async fn setup() -> Harness {
    setup_with(tracking_permissions(false), tracking_permissions(false)).await
}

async fn setup_with(issued: &[&str], allowlist: &'static [&'static str]) -> Harness {
    let mut identity =
        SqliteAuthServer::new(TEST_KEY, TokenConfig::new("brnavi-tracking")).unwrap();
    identity.set_db_host(":memory:");
    identity.initialize_storage().await.unwrap();
    for permission in issued {
        identity
            .handle_add_permission(TRACKING_ROLE.into(), (*permission).into())
            .await
            .unwrap();
    }
    let mut users = Vec::new();
    let mut refresh_tokens = Vec::new();
    for (email, provisioned) in [
        ("alice@example.test", true),
        ("bob@example.test", true),
        ("carol@example.test", false),
    ] {
        let user = identity
            .handle_register(RegisterRequest {
                email: email.into(),
                password: "tracking-test-password".into(),
                profile: HashMap::new(),
            })
            .await
            .unwrap();
        if provisioned {
            identity
                .handle_add_role(user.id.clone(), TRACKING_ROLE.into())
                .await
                .unwrap();
        }
        let tokens = identity
            .handle_login(LoginRequest {
                email: email.into(),
                password: "tracking-test-password".into(),
                grant_type: "password".into(),
            })
            .await
            .unwrap();
        users.push((user.id, tokens.access_token));
        refresh_tokens.push(tokens.refresh_token);
    }
    for permission in allowlist {
        if !issued.contains(permission) {
            identity
                .handle_add_permission(TRACKING_ROLE.into(), (*permission).into())
                .await
                .unwrap();
        }
    }

    let sessions = Arc::new(FakeSessions::default());
    let grants = Arc::new(FakeGrants::default());
    let now = Arc::new(AtomicI64::new(NOW));
    let clock_now = now.clone();
    let service = TrackingService::with_clock(
        sessions.clone(),
        grants.clone(),
        DEFAULT_SESSION_IDLE_TIMEOUT,
        Arc::new(move || clock_now.load(Ordering::SeqCst)),
    );
    let storage = Arc::new(InMemoryCoopNavStorage::development_default());
    let permissions = Arc::new(TestPermissions {
        identity: identity.clone(),
        subjects: Mutex::new(Vec::new()),
        fail: AtomicBool::new(false),
    });
    let auth = AuthPlugin::new(identity.clone());
    let server = RpcServer::new();
    json_rpc_2_oauth2::register_auth_methods(identity.clone(), &server).unwrap();
    register(
        &auth,
        &server,
        service.clone(),
        permissions.clone(),
        allowlist,
    )
    .unwrap();
    CoopNavPlugin::new(
        auth,
        Arc::new(HostSessionAuthorizer(service)),
        storage.clone(),
    )
    .install_methods(&server)
    .unwrap();
    Harness {
        server,
        sessions,
        grants,
        storage,
        now,
        users,
        refresh_tokens,
        identity,
        permissions,
    }
}

async fn call(server: &RpcServer, method: &str, params: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .unwrap();
    serde_json::from_slice(&server.handle_bytes(&bytes).await.unwrap()).unwrap()
}

async fn open(h: &Harness, user: usize, device: &str) -> Value {
    call(
        &h.server,
        "tracking.open",
        json!({ "access_token": h.users[user].1, "deviceId": device }),
    )
    .await
}

fn point(id: &str, latitude: f64) -> Value {
    json!({
        "id": id,
        "smoothedLatitude": latitude,
        "smoothedLongitude": 30.5234,
        "smoothedAccuracyMeters": 5.0,
        "deviceTimeMs": NOW,
        "observedAtMs": NOW,
        "source": "fused",
        "isMock": false,
        "flags": []
    })
}

fn ingest_params(token: &str, opened: &Value, points: Vec<Value>) -> Value {
    json!({
        "access_token": token,
        "sessionId": opened["sessionId"],
        "driverId": opened["driverId"],
        "brandId": opened["brandId"],
        "points": points,
    })
}

fn error_code(response: &Value) -> i64 {
    response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("{response}"))
}

#[tokio::test]
async fn open_binds_registered_device_to_verified_subject() {
    let h = setup().await;
    let alice = h.users[0].0.clone();
    h.sessions.login(&alice, DEVICE, "sid-a1", NOW);

    let response = open(&h, 0, DEVICE).await;
    let result = &response["result"];
    assert_eq!(result["trackerId"], DEVICE, "{response}");
    assert_eq!(result["driverId"], alice);
    assert_eq!(result["brandId"], format!("personal:{alice}"));
    let session_id = result["sessionId"].as_str().unwrap();
    assert_eq!(session_id.len(), 32);
    assert!(session_id.bytes().all(|b| b.is_ascii_hexdigit()));

    let time = call(
        &h.server,
        "serverTime",
        json!({ "access_token": h.users[0].1, "sessionId": session_id }),
    )
    .await;
    assert!(time["result"]["serverTimeMs"].is_number(), "{time}");
}

#[tokio::test]
async fn unregistered_or_foreign_device_is_denied() {
    let h = setup().await;
    let bob = h.users[1].0.clone();
    h.sessions.login(&bob, DEVICE, "sid-b1", NOW);

    assert_eq!(
        error_code(&open(&h, 0, "never-logged-in").await),
        DENIED_CODE
    );
    // Bob's device registration does not authorize Alice's token.
    assert_eq!(error_code(&open(&h, 0, DEVICE).await), DENIED_CODE);
    assert!(h.grants.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_open_requests_are_rejected() {
    let h = setup().await;
    let alice = h.users[0].0.clone();
    h.sessions.login(&alice, DEVICE, "sid-a1", NOW);
    let token = h.users[0].1.clone();
    for params in [
        json!({ "deviceId": DEVICE }),
        json!({ "access_token": "not-a-token", "deviceId": DEVICE }),
    ] {
        let response = call(&h.server, "tracking.open", params).await;
        assert!(response.get("error").is_some(), "{response}");
        assert!(response.get("result").is_none());
    }
    for params in [
        json!({ "access_token": token }),
        json!({ "access_token": token, "deviceId": "bad id/with spaces" }),
        json!({ "access_token": token, "deviceId": "x".repeat(129) }),
        json!({ "access_token": token, "deviceId": DEVICE, "brandId": "fleet-1" }),
    ] {
        assert_eq!(
            error_code(&call(&h.server, "tracking.open", params).await),
            -32602
        );
    }
}

#[tokio::test]
async fn repeat_open_is_stable_and_isolated_per_owner() {
    let h = setup().await;
    let (alice, bob) = (h.users[0].0.clone(), h.users[1].0.clone());
    // The same installation id may be reused by two accounts on one phone.
    h.sessions.login(&alice, DEVICE, "sid-a1", NOW);
    h.sessions.login(&bob, DEVICE, "sid-b1", NOW);

    let first = open(&h, 0, DEVICE).await["result"].clone();
    let again = open(&h, 0, DEVICE).await["result"].clone();
    assert_eq!(first, again);
    let bobs = open(&h, 1, DEVICE).await["result"].clone();
    assert_ne!(bobs["sessionId"], first["sessionId"]);
    assert_eq!(bobs["brandId"], format!("personal:{bob}"));
    assert_eq!(open(&h, 0, DEVICE).await["result"], first);
    assert_eq!(h.grants.0.lock().unwrap().len(), 2);

    // Bob cannot use Alice's sessionId, even with Alice's driver/brand ids.
    let stolen = call(
        &h.server,
        "tracking.ingest",
        ingest_params(&h.users[1].1, &first, vec![point(&"a".repeat(32), 50.45)]),
    )
    .await;
    assert_eq!(error_code(&stolen), DENIED_CODE);
}

#[tokio::test]
async fn relogin_or_idle_expiry_invalidates_tracking_session() {
    let h = setup().await;
    let alice = h.users[0].0.clone();
    let token = h.users[0].1.clone();
    h.sessions.login(&alice, DEVICE, "sid-a1", NOW);
    let first = open(&h, 0, DEVICE).await["result"].clone();
    let ingest = |opened: Value, id: char| {
        call(
            &h.server,
            "tracking.ingest",
            ingest_params(
                &token,
                &opened,
                vec![point(&id.to_string().repeat(32), 50.45)],
            ),
        )
    };
    assert!(ingest(first.clone(), 'a').await.get("result").is_some());

    // A new login on the same device replaces the OAuth sid (revocation).
    h.sessions.login(&alice, DEVICE, "sid-a2", NOW);
    assert_eq!(error_code(&ingest(first.clone(), 'b').await), DENIED_CODE);
    let second = open(&h, 0, DEVICE).await["result"].clone();
    assert_ne!(second["sessionId"], first["sessionId"]);
    assert!(ingest(second.clone(), 'c').await.get("result").is_some());

    // An idle session older than the refresh lifetime can no longer refresh.
    let idle = i64::try_from(DEFAULT_SESSION_IDLE_TIMEOUT.as_millis()).unwrap();
    h.now.store(NOW + idle + 1, Ordering::SeqCst);
    assert_eq!(error_code(&ingest(second, 'd').await), DENIED_CODE);
    assert_eq!(error_code(&open(&h, 0, DEVICE).await), DENIED_CODE);
}

#[tokio::test]
async fn ingest_is_idempotent_and_validates_batches() {
    let h = setup().await;
    let alice = h.users[0].0.clone();
    let token = h.users[0].1.clone();
    h.sessions.login(&alice, DEVICE, "sid-a1", NOW);
    let opened = open(&h, 0, DEVICE).await["result"].clone();
    let batch = vec![point(&"1".repeat(32), 50.45), point(&"2".repeat(32), 50.46)];

    for _ in 0..2 {
        let response = call(
            &h.server,
            "tracking.ingest",
            ingest_params(&token, &opened, batch.clone()),
        )
        .await;
        assert_eq!(
            response["result"]["acceptedIds"],
            json!(["1".repeat(32), "2".repeat(32)]),
            "{response}"
        );
    }
    let partition = StoragePartition {
        brand_id: format!("personal:{alice}"),
        driver_id: alice.clone(),
    };
    assert_eq!(h.storage.stream_snapshot(&partition).len(), 2);

    let conflict = call(
        &h.server,
        "tracking.ingest",
        ingest_params(&token, &opened, vec![point(&"1".repeat(32), 10.0)]),
    )
    .await;
    assert!(conflict.get("error").is_some(), "{conflict}");

    let too_many = (0..=json_rpc_2_coopnav::MAX_BATCH_POINTS)
        .map(|i| point(&format!("{i:032x}"), 50.45))
        .collect();
    assert_eq!(
        error_code(
            &call(
                &h.server,
                "tracking.ingest",
                ingest_params(&token, &opened, too_many)
            )
            .await
        ),
        -32602
    );
    assert_eq!(
        error_code(
            &call(
                &h.server,
                "tracking.ingest",
                ingest_params(&token, &opened, Vec::new())
            )
            .await
        ),
        -32602
    );

    let mut foreign_brand = opened.clone();
    foreign_brand["brandId"] = json!("fleet-1");
    assert_eq!(
        error_code(
            &call(
                &h.server,
                "tracking.ingest",
                ingest_params(&token, &foreign_brand, vec![point(&"3".repeat(32), 50.45)])
            )
            .await
        ),
        DENIED_CODE
    );
}

#[tokio::test]
async fn existing_user_gets_only_tracking_permissions_after_standard_refresh() {
    let h = setup().await;
    let carol = h.users[2].0.clone();
    h.sessions.login(&carol, DEVICE, "sid-c1", NOW);
    let response = open(&h, 2, DEVICE).await;
    assert!(response.get("error").is_none(), "{response}");
    let opened = response["result"].clone();
    assert_eq!(opened["refreshRequired"], true);
    // Repeated opens with the stale token are deterministic and idempotent.
    assert_eq!(open(&h, 2, DEVICE).await, response);
    let profile = h.identity.handle_get_profile(carol.clone()).await.unwrap();
    assert_eq!(profile.roles, vec![TRACKING_ROLE]);
    assert!(
        h.identity
            .authenticate_access_token(&h.users[2].1)
            .unwrap()
            .permissions
            .is_empty()
    );
    let refreshed = call(
        &h.server,
        "auth.refresh",
        json!({ "refresh_token": h.refresh_tokens[2] }),
    )
    .await;
    let token = refreshed["result"]["access_token"].as_str().unwrap();
    let claims = h.identity.authenticate_access_token(token).unwrap();
    let mut expected = tracking_permissions(false)
        .iter()
        .map(|permission| (*permission).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(claims.permissions, expected);
    assert_eq!(opened["driverId"], carol);
    assert_eq!(opened["brandId"], personal_brand(&carol));
    let params = ingest_params(token, &opened, vec![point(&"a".repeat(32), 50.45)]);
    // No implicit upgrade of the old token: CoopNav still enforces its scopes.
    let mut stale = params.clone();
    stale["access_token"] = json!(h.users[2].1);
    assert_eq!(
        error_code(&call(&h.server, "tracking.ingest", stale).await),
        -32003
    );
    let response = call(&h.server, "tracking.ingest", params.clone()).await;
    assert_eq!(response["result"]["acceptedIds"], json!(["a".repeat(32)]));
    assert_eq!(call(&h.server, "tracking.ingest", params).await, response);
    let time = call(
        &h.server,
        "serverTime",
        json!({ "access_token": token, "sessionId": opened["sessionId"] }),
    )
    .await;
    assert!(time["result"]["serverTimeMs"].is_i64(), "{time}");
    // The original grant already worked after refresh, without another open.
    let reopened = call(
        &h.server,
        "tracking.open",
        json!({ "authorization": format!("Bearer {token}"), "deviceId": DEVICE }),
    )
    .await["result"]
        .clone();
    assert!(reopened.get("refreshRequired").is_none());
    assert_eq!(reopened["sessionId"], opened["sessionId"]);
    for permission in ["rbac.users.write", "rbac.roles.write", "garage.admin", "*"] {
        assert!(h.identity.require_permission(token, permission).is_err());
    }
    assert_eq!(
        error_code(
            &call(
                &h.server,
                "tracking.ingest",
                ingest_params(&h.users[1].1, &opened, vec![point(&"b".repeat(32), 50.45)])
            )
            .await
        ),
        DENIED_CODE
    );
}

#[tokio::test]
async fn new_user_provisioning_preserves_other_roles_and_relogin_loads_permissions() {
    let h = setup().await;
    let registered = call(
        &h.server,
        "auth.register",
        json!({
            "email": "new@example.test",
            "password": "tracking-test-password",
            "profile": {
                "first_name": "New", "last_name": "User", "account_type": "private",
            },
        }),
    )
    .await;
    assert!(registered.get("error").is_none(), "{registered}");
    let subject = registered["result"]["id"].as_str().unwrap();
    h.identity
        .handle_add_role(subject.into(), "existing.reader".into())
        .await
        .unwrap();
    let login_params = json!({
        "email": "new@example.test",
        "password": "tracking-test-password",
        "grant_type": "password",
        "device": { "deviceId": DEVICE, "os": { "name": "Android", "version": "16" } },
    });
    let login = call(&h.server, "auth.login", login_params.clone()).await;
    let token = login["result"]["access_token"].as_str().unwrap();
    h.sessions.login(subject, DEVICE, "sid-new", NOW);
    let initial_open = call(
        &h.server,
        "tracking.open",
        json!({ "access_token": token, "deviceId": DEVICE }),
    )
    .await["result"]
        .clone();
    assert_eq!(initial_open["refreshRequired"], true);
    let profile = h.identity.handle_get_profile(subject.into()).await.unwrap();
    assert_eq!(profile.roles.len(), 2);
    assert!(profile.roles.contains(&"existing.reader".to_owned()));
    assert!(profile.roles.contains(&TRACKING_ROLE.to_owned()));
    let refreshed = call(
        &h.server,
        "auth.refresh",
        json!({ "refresh_token": login["result"]["refresh_token"] }),
    )
    .await;
    let refreshed_token = refreshed["result"]["access_token"].as_str().unwrap();
    let opened = call(
        &h.server,
        "tracking.open",
        json!({ "access_token": refreshed_token, "deviceId": DEVICE }),
    )
    .await;
    assert_eq!(opened["result"]["driverId"], subject);
    assert_eq!(opened["result"]["sessionId"], initial_open["sessionId"]);
    assert!(opened["result"].get("refreshRequired").is_none());
    let login = call(&h.server, "auth.login", login_params).await;
    let token = login["result"]["access_token"].as_str().unwrap();
    let opened = call(
        &h.server,
        "tracking.open",
        json!({ "access_token": token, "deviceId": DEVICE }),
    )
    .await;
    assert_eq!(opened["result"]["trackerId"], DEVICE);
    assert_eq!(opened["result"]["driverId"], subject);
    assert!(opened["result"].get("refreshRequired").is_none());
}

#[tokio::test]
async fn denied_open_never_provisions_and_provisioning_errors_fail_closed() {
    let h = setup().await;
    h.sessions.login(&h.users[0].0, DEVICE, "sid-a", NOW);
    assert_eq!(error_code(&open(&h, 2, DEVICE).await), DENIED_CODE);
    assert!(
        call(
            &h.server,
            "tracking.open",
            json!({ "access_token": "invalid", "deviceId": DEVICE })
        )
        .await
        .get("error")
        .is_some()
    );
    assert_eq!(error_code(&open(&h, 0, "invalid device").await), -32602);
    assert!(h.permissions.subjects.lock().unwrap().is_empty());
    let carol = &h.users[2].0;
    h.sessions.login(carol, DEVICE, "sid-c", NOW);
    h.permissions.fail.store(true, Ordering::SeqCst);
    assert_eq!(error_code(&open(&h, 2, DEVICE).await), -32603);
    assert!(
        h.identity
            .handle_get_profile(carol.clone())
            .await
            .unwrap()
            .roles
            .is_empty()
    );
    assert_eq!(*h.permissions.subjects.lock().unwrap(), vec![carol.clone()]);
}

#[test]
fn host_role_must_contain_exactly_the_tracking_allowlist() {
    for allowlist in [tracking_permissions(false), tracking_permissions(true)] {
        assert!(tracking_role_is_safe(
            &doc! { "name": TRACKING_ROLE, "permissions": allowlist.to_vec() },
            allowlist
        ));
        let mut reversed = allowlist.to_vec();
        reversed.reverse();
        assert!(tracking_role_is_safe(
            &doc! { "name": TRACKING_ROLE, "permissions": reversed },
            allowlist
        ));
        assert!(!tracking_role_is_safe(
            &doc! { "name": TRACKING_ROLE, "permissions": allowlist.to_vec() },
            if allowlist.len() == 2 {
                tracking_permissions(true)
            } else {
                tracking_permissions(false)
            }
        ));
        let write = tracking_role_write(allowlist);
        assert!(write.get_document("$set").is_ok());
        assert!(!write.contains_key("$setOnInsert"));
        assert_eq!(
            write
                .get_document("$set")
                .unwrap()
                .get_array("permissions")
                .unwrap()
                .len(),
            allowlist.len()
        );
    }
    let stage1 = tracking_permissions(false);
    for permissions in [
        vec!["*"],
        vec![stage1[0]],
        vec![stage1[0], stage1[0]],
        vec![stage1[0], stage1[1], "rbac.roles.write"],
        tracking_permissions(true).to_vec(),
    ] {
        assert!(!tracking_role_is_safe(
            &doc! { "name": TRACKING_ROLE, "permissions": permissions },
            stage1
        ));
    }
    assert!(!tracking_role_is_safe(
        &doc! { "name": "other", "permissions": stage1.to_vec() },
        stage1
    ));
    assert!(!tracking_role_is_safe(
        &doc! { "name": TRACKING_ROLE },
        stage1
    ));
}

#[tokio::test]
async fn stage2_open_asks_for_refresh_until_coop_sync_is_on_the_token() {
    let h = setup_with(tracking_permissions(false), tracking_permissions(true)).await;
    let alice = h.users[0].0.clone();
    h.sessions.login(&alice, DEVICE, "sid-a", NOW);
    let opened = open(&h, 0, DEVICE).await;
    assert!(opened.get("error").is_none(), "{opened}");
    let result = opened["result"].clone();
    assert_eq!(
        result
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.as_str() != "refreshRequired")
            .count(),
        4
    );
    assert_eq!(result["refreshRequired"], true);
    assert_eq!(result["brandId"], personal_brand(&alice));
    let ingest = ingest_params(&h.users[0].1, &result, vec![point(&"c".repeat(32), 50.45)]);
    assert_eq!(
        call(&h.server, "tracking.ingest", ingest).await["result"]["acceptedIds"],
        json!(["c".repeat(32)])
    );
    let stale = call_transport(
        &h.server,
        "coop.sync_peers",
        json!({ "access_token": h.users[0].1 }),
        Some("quic"),
    )
    .await;
    assert_eq!(error_code(&stale), -32003);
    assert_eq!(
        error_code(
            &call_transport(
                &h.server,
                "coop.sync_peers",
                json!({ "access_token": h.users[0].1 }),
                Some("tcp"),
            )
            .await
        ),
        -32014
    );

    let refreshed = call(
        &h.server,
        "auth.refresh",
        json!({ "refresh_token": h.refresh_tokens[0] }),
    )
    .await;
    let token = refreshed["result"]["access_token"].as_str().unwrap();
    let mut expected = tracking_permissions(true)
        .iter()
        .map(|permission| (*permission).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        h.identity
            .authenticate_access_token(token)
            .unwrap()
            .permissions,
        expected
    );
    let again = call(
        &h.server,
        "tracking.open",
        json!({ "access_token": token, "deviceId": DEVICE }),
    )
    .await["result"]
        .clone();
    assert!(again.get("refreshRequired").is_none());
    assert_eq!(again["sessionId"], result["sessionId"]);
    let permitted = call_transport(
        &h.server,
        "coop.sync_peers",
        json!({ "access_token": token }),
        Some("quic"),
    )
    .await;
    assert_ne!(error_code(&permitted), -32003);
    assert_eq!(
        error_code(
            &call_transport(
                &h.server,
                "coop.sync_peers",
                json!({ "access_token": token }),
                None,
            )
            .await
        ),
        -32014
    );
}

async fn call_transport(
    server: &RpcServer,
    method: &str,
    params: Value,
    transport: Option<&str>,
) -> Value {
    let bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .unwrap();
    let response = match transport {
        Some(transport) => {
            let context = TransportContext {
                transport: transport.into(),
                connection_id: "conn-1".into(),
                push_sender: None,
            };
            server
                .handle_message_with_context(&bytes, &context)
                .await
                .unwrap()
        }
        None => server.handle_bytes(&bytes).await.unwrap(),
    };
    serde_json::from_slice(&response).unwrap()
}

#[test]
fn stage2_env_stays_closed_until_the_flag_and_shared_index_are_set() {
    let mut seen = Vec::new();
    let disabled = stage2_from_vars(|name| {
        seen.push(name.to_owned());
        Ok(None)
    })
    .unwrap();
    assert!(disabled.is_none());
    assert_eq!(seen, ["COOPNAV_STAGE2"]);

    let explicit = stage2_from_vars(|name| {
        seen.push(name.to_owned());
        Ok(Some("false".into()))
    })
    .unwrap();
    assert!(explicit.is_none());
    assert_eq!(seen, ["COOPNAV_STAGE2", "COOPNAV_STAGE2"]);

    let missing_epoch = stage2_from_vars(|name| {
        Ok(match name {
            "COOPNAV_STAGE2" => Some("true".into()),
            _ => None,
        })
    })
    .unwrap_err();
    assert!(missing_epoch.to_string().contains("COOPNAV_EPOCH"));

    let configured = stage2_from_vars(|name| {
        Ok(match name {
            "COOPNAV_STAGE2" => Some("true".into()),
            "COOPNAV_EPOCH" => Some("deploy-1".into()),
            "COOPNAV_COOP_BRAND" => Some("city".into()),
            "COOPNAV_REDIS_URL" => Some("redis://127.0.0.1:6379/".into()),
            "COOPNAV_GTFS_URL" => Some("https://transit.example/gtfs".into()),
            _ => None,
        })
    })
    .unwrap()
    .unwrap();
    assert!(configured.gtfs.is_none());
    assert_eq!(configured.epoch, "deploy-1");
    assert_eq!(configured.brand, "city");

    let feed = stage2_from_vars(|name| {
        Ok(Some(
            match name {
                "COOPNAV_STAGE2" => "true",
                "COOPNAV_EPOCH" => "deploy-1",
                "COOPNAV_COOP_BRAND" => "city",
                "COOPNAV_REDIS_URL" => "redis://127.0.0.1:6379/",
                "COOPNAV_GTFS_URL" => "https://transit.example/gtfs",
                "COOPNAV_GTFS_ROUTES" => "bus:12,trolleybus:3",
                "COOPNAV_GTFS_ACCURACY_METERS" => "8",
                "COOPNAV_GTFS_TRUST_SCORE" => "0.9",
                _ => unreachable!(),
            }
            .into(),
        ))
    })
    .unwrap()
    .unwrap();
    let gtfs = feed.gtfs.unwrap();
    assert_eq!(gtfs.brand_id, "city");
    assert_eq!(gtfs.route_classes.len(), 2);

    let car = stage2_from_vars(|name| {
        Ok(Some(
            match name {
                "COOPNAV_STAGE2" => "true",
                "COOPNAV_EPOCH" => "deploy-1",
                "COOPNAV_COOP_BRAND" => "city",
                "COOPNAV_REDIS_URL" => "redis://127.0.0.1:6379/",
                "COOPNAV_GTFS_URL" => "https://transit.example/gtfs",
                "COOPNAV_GTFS_ROUTES" => "car:1",
                "COOPNAV_GTFS_ACCURACY_METERS" => "8",
                "COOPNAV_GTFS_TRUST_SCORE" => "0.9",
                _ => unreachable!(),
            }
            .into(),
        ))
    })
    .unwrap_err();
    assert!(car.to_string().contains("bus and trolleybus"));
}

#[test]
fn mongo_documents_round_trip_and_partition_by_owner() {
    let point: TrackPoint = serde_json::from_value(json!({
        "id": "0123456789abcdef0123456789abcdef",
        "rawLatitude": 50.4501, "rawLongitude": 30.5234, "rawAccuracyMeters": 7.25,
        "smoothedLatitude": 50.45011, "smoothedLongitude": 30.52341,
        "smoothedAccuracyMeters": 4.5, "speedMps": 12.5, "bearingDegrees": 90.0,
        "deviceTimeMs": NOW, "observedAtMs": NOW + 1,
        "source": "gps", "isMock": false, "flags": ["x"]
    }))
    .unwrap();
    let alice = StoragePartition {
        brand_id: "personal:alice".into(),
        driver_id: "alice".into(),
    };
    let bob = StoragePartition {
        brand_id: "personal:bob".into(),
        driver_id: "bob".into(),
    };
    let document = point_document(&alice, &point).unwrap();
    assert_eq!(stored_point(&document).unwrap(), point);
    assert_eq!(
        document.get_document("_id").unwrap(),
        &point_key(&alice, &point.id)
    );
    assert_ne!(point_key(&alice, &point.id), point_key(&bob, &point.id));

    let grant = doc! { "_id": grant_key("alice", DEVICE, "sid-1"), "session_id": "s" };
    assert_eq!(
        grant_from_document(&grant),
        Some(TrackingGrant {
            session_id: "s".into(),
            subject: "alice".into(),
            device_id: DEVICE.into(),
            auth_sid: "sid-1".into(),
        })
    );
}
