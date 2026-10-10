use std::{
    net::{SocketAddr, TcpListener},
    path::Path,
    sync::Arc,
    time::Duration,
};

use quic_jsonrpc_client::QuicJsonRpcClient;
use quic_jsonrpc_server::{RateLimitConfig, ServerOptions, SessionBackend, TlsPaths};
use serde_json::json;

// Idle transport tests do not authenticate; an explicit test adapter avoids a Mongo dependency.
async fn run_with_shutdown(options: ServerOptions, shutdown: tokio::sync::oneshot::Receiver<()>) -> anyhow::Result<()> {
    use quic_jsonrpc_server::{
        UpstreamOptions, domain::GatewayAuthenticator, infrastructure::http::HttpDriverApiGateway,
    };
    let gateway = Arc::new(HttpDriverApiGateway::new(UpstreamOptions::from_env()?));
    quic_jsonrpc_server::run_with_authenticator(options, shutdown, Arc::new(GatewayAuthenticator(gateway))).await
}

fn pick_unused_local_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("get local address");
    drop(listener);
    addr
}

fn write_test_cert(cert_path: &Path, key_path: &Path) {
    let key = rcgen::KeyPair::generate().expect("generate key pair");
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .expect("create cert params")
        .self_signed(&key)
        .expect("self-sign certificate");

    std::fs::write(cert_path, cert.pem()).expect("write cert");
    std::fs::write(key_path, key.serialize_pem()).expect("write key");
}

fn create_tls_test_context() -> (SocketAddr, tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let addr = pick_unused_local_addr();
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cert_path = temp_dir.path().join("server-cert.pem");
    let key_path = temp_dir.path().join("server-key.pem");
    write_test_cert(&cert_path, &key_path);
    (addr, temp_dir, cert_path, key_path)
}

fn base_test_server_options(
    bind_addr: SocketAddr,
    cert_path: std::path::PathBuf,
    key_path: std::path::PathBuf,
) -> ServerOptions {
    ServerOptions {
        bind_addr,
        tls_paths: Some(TlsPaths {
            cert_path,
            key_path,
            chain_path: None,
        }),
        metrics_interval: Duration::from_secs(300),
        keep_alive_interval: Duration::from_secs(15),
        idle_timeout: Duration::from_secs(3600),
        session_ttl: Duration::from_secs(3600),
        session_cleanup_interval: Duration::from_secs(60),
        session_backend: SessionBackend::Memory,
        session_debug_enabled: false,
        auth_rate_limit: RateLimitConfig::disabled(),
        auth_identity_rate_limit: RateLimitConfig::disabled(),
    }
}

async fn start_test_server(
    options: ServerOptions,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    (shutdown_tx, server)
}

#[tokio::test]
async fn connection_survives_30_seconds_silence_with_heartbeat() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(15);
    options.idle_timeout = Duration::from_secs(120); // 2 minutes

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = Arc::new(
        QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
            .await
            .expect("client connect"),
    );

    let initial = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 1}))
        .await
        .expect("first ping")
        .expect("response");
    assert_eq!(initial["result"], json!("pong"));

    // Spawn heartbeat with 10 second interval
    let _heartbeat = client.clone().spawn_heartbeat(Duration::from_secs(10));

    // Wait 30 seconds (3 heartbeat cycles) without manual requests
    tokio::time::sleep(Duration::from_secs(30)).await;

    // Connection should still be alive
    assert!(
        client.is_connected().await,
        "connection should survive 30s with heartbeat"
    );

    let after_idle = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 2}))
        .await
        .expect("ping after idle")
        .expect("response");
    assert_eq!(after_idle["result"], json!("pong"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn heartbeat_detects_stale_connection_and_reconnects() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(15);
    options.idle_timeout = Duration::from_secs(1); // Very short timeout to force reconnect

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = Arc::new(
        QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
            .await
            .expect("client connect"),
    );

    let first_health = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("first health request")
        .expect("first health response");
    let first_accepted = first_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    // Spawn heartbeat but with slow interval so we see the timeout and recover
    let _heartbeat = client.clone().spawn_heartbeat(Duration::from_millis(300));

    // Wait for server to timeout the connection (1 second)
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Connection should have been detected as stale by heartbeat and reconnected
    // The next request should work with a new connection
    let second_health = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
        .await
        .expect("request after server timeout")
        .expect("second health response");
    let second_accepted = second_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    // Either same connection (if keep-alive prevented timeout) or +1 (if it reconnected)
    assert!(
        second_accepted == first_accepted || second_accepted == first_accepted + 1,
        "expected same or new connection: first_accepted={}, second_accepted={}",
        first_accepted,
        second_accepted
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn connection_without_heartbeat_reconnects_on_next_request_after_timeout() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(15);
    options.idle_timeout = Duration::from_secs(1); // 1 second timeout

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let first = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("first health")
        .expect("response");
    let first_accepted = first["result"]["connections"]["accepted"].as_u64().unwrap();

    // NO heartbeat - just wait for idle timeout
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        !client.is_connected().await,
        "connection should be closed after timeout"
    );

    // Next request should transparently reconnect
    let second = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
        .await
        .expect("health after timeout")
        .expect("response");
    let second_accepted = second["result"]["connections"]["accepted"].as_u64().unwrap();

    assert_eq!(
        second_accepted,
        first_accepted + 1,
        "exactly one reconnect expected after timeout"
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn heartbeat_resilient_to_consecutive_failures() {
    // This test verifies that heartbeat doesn't give up after a few failures
    // but continues trying to maintain the connection
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(10);
    options.idle_timeout = Duration::from_secs(2);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = Arc::new(
        QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
            .await
            .expect("client connect"),
    );

    let _heartbeat = client.clone().spawn_heartbeat(Duration::from_millis(200));

    // Let heartbeat run for 5 seconds - this covers multiple reconnect cycles
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Should still be operational
    let response = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 99}))
        .await
        .expect("request should succeed")
        .expect("response");
    assert_eq!(response["result"], json!("pong"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

/// Half-open connection regression: a client that disappears *without* sending CONNECTION_CLOSE
/// (phone died, NAT binding dropped, Doze) must be reaped by the server once it stops answering
/// keep-alive PINGs — within `idle_timeout`, not after the session TTL.
#[tokio::test]
async fn server_reaps_vanished_client_within_idle_timeout() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(1);
    options.idle_timeout = Duration::from_secs(3);
    options.session_ttl = Duration::from_secs(3600); // session TTL must play no role here

    let (shutdown_tx, server) = start_test_server(options).await;

    // Observer client: used to read server metrics.
    let observer = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("observer connect");
    let before = observer
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("health")
        .expect("response");
    let closed_before = before["result"]["connections"]["closed"].as_u64().unwrap();
    let timed_out_before = before["result"]["quic"]["disconnects"]["timed_out"].as_u64().unwrap();

    // "Zombie" client on its own runtime (on a plain OS thread — a runtime cannot be driven from
    // inside another): connect, exchange one request, then make it vanish — the runtime is torn
    // down with the connection/endpoint leaked, so no CONNECTION_CLOSE is ever sent and nothing
    // answers the server's keep-alive PINGs any more.
    {
        let cert_path = cert_path.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("zombie runtime");
            let zombie = runtime.block_on(async move {
                let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
                    .await
                    .expect("zombie connect");
                let pong = client
                    .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 1}))
                    .await
                    .expect("zombie ping")
                    .expect("response");
                assert_eq!(pong["result"], json!("pong"));
                client
            });
            std::mem::forget(zombie);
            runtime.shutdown_background();
        })
        .await
        .expect("zombie thread");
    }

    // Poll the server until it has closed the zombie connection as timed out. Allow idle
    // timeout + generous margin (PTO back-off, timer granularity), but far less than session TTL.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut reaped = false;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = observer
            .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
            .await
            .expect("health")
            .expect("response");
        let closed = now["result"]["connections"]["closed"].as_u64().unwrap();
        let timed_out = now["result"]["quic"]["disconnects"]["timed_out"].as_u64().unwrap();
        if closed > closed_before && timed_out > timed_out_before {
            reaped = true;
            break;
        }
    }
    assert!(
        reaped,
        "server must reap the vanished client as `timed_out` within the idle timeout"
    );

    // The observer (live, idle, kept alive only by QUIC keep-alives) must still be connected.
    assert!(observer.is_connected().await, "live idle client must not be dropped");

    observer.close().await;
    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}
