use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::Path,
    sync::Arc,
    time::Duration,
};

use once_cell::sync::Lazy;
use quic_jsonrpc_client::{QuicJsonRpcClient, RebindOutcome};
use quic_jsonrpc_server::{RateLimitConfig, ServerOptions, SessionBackend, TlsPaths};
use serde_json::{Value, json};

/// Serialises tests that mutate the process environment. Async-aware so the guard may be held
/// across `.await` points.
static ENV_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

// These transport/legacy upstream contract tests inject their mock HTTP authenticator.
// Production startup always uses MongoDB, including with in-memory session storage.
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

fn read_http_request(stream: &mut std::net::TcpStream) -> (String, Value) {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 1024];
    let mut headers_end = None;
    let mut content_length = 0_usize;

    loop {
        let n = stream.read(&mut chunk).expect("read request bytes");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);

        if headers_end.is_none() {
            headers_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
            if let Some(end) = headers_end {
                let headers = String::from_utf8_lossy(&buf[..end]);
                content_length = headers
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        if lower.starts_with("content-length:") {
                            line.split(':').nth(1)?.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
            }
        }

        if let Some(end) = headers_end {
            let body_len = buf.len().saturating_sub(end);
            if body_len >= content_length {
                break;
            }
        }
    }

    let headers_end = headers_end.expect("http headers end");
    let headers = String::from_utf8(buf[..headers_end].to_vec()).expect("utf8 headers");
    let body = &buf[headers_end..headers_end + content_length];
    let body_json: Value = serde_json::from_slice(body).expect("json body");
    (headers, body_json)
}

fn spawn_mock_upstream() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock addr");

    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept upstream conn");
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .expect("set timeout");

            let (headers, body) = read_http_request(&mut stream);

            assert!(
                headers.to_ascii_lowercase().contains("authorization: bearer token-123"),
                "missing bearer header: {headers}"
            );

            let response_json = if headers.starts_with("POST /driver/shift/start ")
                || headers.starts_with("POST /driver/shift/start/ ")
            {
                assert_eq!(body["deviceTime"], json!("2026-09-15T10:11:12Z"));
                assert_eq!(body["sendTime"], json!(1726395072));
                assert_eq!(body["vehicleId"], json!("vehicle-123"));
                assert_eq!(body["lat"], json!(50.4501));
                assert_eq!(body["lng"], json!(30.5234));
                json!({"status": "ok", "kind": "start"})
            } else if headers.starts_with("POST /driver/shift/finish ")
                || headers.starts_with("POST /driver/shift/finish/ ")
            {
                assert_eq!(body["deviceTime"], json!("2026-09-15T10:11:20Z"));
                json!({"status": "ok", "kind": "finish"})
            } else {
                panic!("unexpected request line: {headers}");
            };

            let response_body = response_json.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).expect("write response");
        }
    });

    (format!("http://{addr}"), handle)
}

fn spawn_mock_finish_shift_error_upstream() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock addr");

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept upstream conn");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set timeout");

        let (headers, body) = read_http_request(&mut stream);

        assert!(
            headers.to_ascii_lowercase().contains("authorization: bearer token-123"),
            "missing bearer header: {headers}"
        );
        assert!(
            headers.starts_with("POST /driver/shift/finish ") || headers.starts_with("POST /driver/shift/finish/ "),
            "unexpected request line: {headers}"
        );
        assert_eq!(body["deviceTime"], json!("2026-09-15T10:11:20Z"));

        let response_body = json!({
            "error": {
                "message": "Зміну вже завершено"
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).expect("write response");
    });

    (format!("http://{addr}"), handle)
}

fn spawn_mock_finish_shift_empty_error_upstream() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock addr");

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept upstream conn");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set timeout");

        let (headers, body) = read_http_request(&mut stream);

        assert!(
            headers.to_ascii_lowercase().contains("authorization: bearer token-123"),
            "missing bearer header: {headers}"
        );
        assert!(
            headers.starts_with("POST /driver/shift/finish ") || headers.starts_with("POST /driver/shift/finish/ "),
            "unexpected request line: {headers}"
        );
        assert_eq!(body["deviceTime"], json!("2026-09-15T10:11:20Z"));

        let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).expect("write response");
    });

    (format!("http://{addr}"), handle)
}

fn spawn_mock_slow_finish_shift_upstream(delay: Duration) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock addr");

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept upstream conn");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set timeout");

        let (headers, body) = read_http_request(&mut stream);

        assert!(
            headers.to_ascii_lowercase().contains("authorization: bearer token-123"),
            "missing bearer header: {headers}"
        );
        assert!(
            headers.starts_with("POST /driver/shift/finish ") || headers.starts_with("POST /driver/shift/finish/ "),
            "unexpected request line: {headers}"
        );
        assert_eq!(body["deviceTime"], json!("2026-09-15T10:11:20Z"));

        std::thread::sleep(delay);

        let response_json = json!({"status": "ok", "kind": "finish"}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_json.len(),
            response_json
        );
        stream.write_all(response.as_bytes()).expect("write response");
    });

    (format!("http://{addr}"), handle)
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: guarded by ENV_LOCK for this integration test file.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: guarded by ENV_LOCK for this integration test file.
        unsafe {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
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

fn create_tls_test_context() -> (SocketAddr, tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let addr = pick_unused_local_addr();
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cert_path = temp_dir.path().join("server-cert.pem");
    let key_path = temp_dir.path().join("server-key.pem");
    write_test_cert(&cert_path, &key_path);
    (addr, temp_dir, cert_path, key_path)
}

fn spawn_mock_login_upstream() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock addr");

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept upstream conn");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set timeout");

        let (headers, body) = read_http_request(&mut stream);
        assert!(
            headers.starts_with("POST /auth/login ") || headers.starts_with("POST /auth/login/ "),
            "unexpected request line: {headers}"
        );
        assert_eq!(body["phone"], json!("+380991112233"));

        // Upstream does not know anything about sessions: only a token is returned.
        let response_body = json!({"status": "ok", "accessToken": "token-123"}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).expect("write response");
    });

    (format!("http://{addr}"), handle)
}

/// Minimal OSM-like tile server: answers `GET /{z}/{x}/{y}.png` with a tiny PNG-tagged body
/// (`\x89PNG` + the path) and 404 for `/9/9/9.png`. Serves `requests` connections then exits;
/// counts them so the test can prove the server-side cache absorbed repeats.
fn spawn_mock_tile_server(
    requests: usize,
) -> (String, Arc<std::sync::atomic::AtomicUsize>, std::thread::JoinHandle<()>) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock tile server");
    let addr = listener.local_addr().expect("mock addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);

    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(false).expect("blocking listener");
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().expect("accept tile conn");
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .expect("set timeout");

            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let n = stream.read(&mut chunk).expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&buf).to_string();
            let path = headers
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_string();
            assert!(
                headers.to_ascii_lowercase().contains("user-agent: tms-e2e-tiles"),
                "tile request must carry the configured User-Agent: {headers}"
            );
            hits_thread.fetch_add(1, Ordering::SeqCst);

            let response = if path == "/9/9/9.png" {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                let mut body = b"\x89PNG\r\n\x1a\n".to_vec();
                body.extend_from_slice(path.as_bytes());
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nETag: \"tile-etag\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(&body);
                response
            };
            stream.write_all(&response).expect("write tile response");
        }
    });

    (format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"), hits, handle)
}

#[tokio::test]
async fn get_tile_roundtrip_decodes_png_and_uses_server_cache() {
    let _env_guard = ENV_LOCK.lock().await;
    // 2 upstream hits: one for the cached tile, one for the 404.
    let (template, hits, upstream) = spawn_mock_tile_server(2);

    let _template_guard = EnvVarGuard::set("MAP_TILE_URL_TEMPLATE", &template);
    let _ua_guard = EnvVarGuard::set("MAP_TILE_USER_AGENT", "tms-e2e-tiles");
    let _retries_guard = EnvVarGuard::set("MAP_TILE_RETRIES", "0");
    let _zoom_guard = EnvVarGuard::set("MAP_TILE_MAX_ZOOM", "19");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();
    let options = base_test_server_options(addr, cert_path.clone(), key_path);
    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    // Typed helper: decoded bytes.
    let tile = client.get_tile(12, 2400, 1400).await.expect("tile");
    assert_eq!((tile.z, tile.x, tile.y), (12, 2400, 1400));
    assert_eq!(tile.content_type, "image/png");
    assert_eq!(tile.etag.as_deref(), Some("\"tile-etag\""));
    assert!(!tile.from_cache);
    assert!(tile.bytes.starts_with(b"\x89PNG"), "{:?}", tile.bytes);
    assert!(tile.bytes.ends_with(b"/12/2400/1400.png"));

    // Same tile again → served from the server cache, upstream untouched.
    let again = client.get_tile(12, 2400, 1400).await.expect("tile");
    assert!(again.from_cache);
    assert_eq!(again.bytes, tile.bytes);

    // Raw JSON-RPC path (what Flutter does through quic_rpc_client_request).
    let raw = client
        .request(
            json!({"jsonrpc": "2.0", "method": "map.getTile", "id": "t", "params": {"z": 12, "x": 2400, "y": 1400}}),
        )
        .await
        .expect("request")
        .expect("response");
    assert_eq!(raw["result"]["fromCache"], json!(true));
    assert!(
        raw["result"]["data"]
            .as_str()
            .is_some_and(|d| d.starts_with("iVBORw0KGgo"))
    );

    // Upstream 404 → -32010 and never cached.
    let err = client.get_tile(9, 9, 9).await.expect_err("404 must fail");
    assert!(err.to_string().contains("-32010"), "{err:#}");

    // Validation happens before the upstream is touched.
    let bad = client
        .request(json!({"jsonrpc": "2.0", "method": "map.getTile", "id": "b", "params": {"z": 3, "x": 8, "y": 0}}))
        .await
        .expect("request")
        .expect("response");
    assert_eq!(bad["error"]["code"], json!(-32602));

    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);

    client.close().await;
    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("mock tile server");
}

#[tokio::test]
async fn ping_roundtrip_works_against_real_server() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 42}))
        .await
        .expect("rpc request")
        .expect("response");

    assert_eq!(response["result"], json!("pong"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn rebind_migrates_live_connection_without_new_handshake() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();
    let options = base_test_server_options(addr, cert_path.clone(), key_path);
    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let health = |id: u64| {
        let client = &client;
        async move {
            client
                .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": id}))
                .await
                .expect("health request")
                .expect("health response")["result"]["connections"]["accepted"]
                .as_u64()
                .expect("accepted counter")
        }
    };
    let accepted_before = health(1).await;

    // Simulates the Android network callback: new socket, same session.
    let outcome = client.rebind(None).await.expect("rebind");
    assert_eq!(outcome, RebindOutcome::Migrated);

    let response = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 2}))
        .await
        .expect("ping after rebind")
        .expect("ping response");
    assert_eq!(response["result"], json!("pong"));

    // Connection migration must not create a new server-side connection.
    assert_eq!(
        health(3).await,
        accepted_before,
        "migration must reuse the QUIC connection"
    );
    assert!(client.is_connected().await);

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn rebind_on_dead_connection_reconnects_and_keeps_client_usable() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();
    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    options.idle_timeout = Duration::from_secs(1);
    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    // Let the server drop the connection on its idle timeout.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        !client.is_connected().await,
        "server should have timed the connection out"
    );

    let outcome = client.rebind(None).await.expect("rebind on dead connection");
    assert_eq!(outcome, RebindOutcome::Reconnected);
    assert!(client.is_connected().await);

    let response = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 7}))
        .await
        .expect("ping after reconnect")
        .expect("ping response");
    assert_eq!(response["result"], json!("pong"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn heartbeat_keeps_connection_alive_without_new_connection() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    options.idle_timeout = Duration::from_secs(1);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = Arc::new(
        QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
            .await
            .expect("client connect"),
    );

    let initial_health = client
        .request(serde_json::json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("initial health request")
        .expect("initial health response");
    let initial_accepted = initial_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    let _heartbeat = client.clone().spawn_heartbeat(Duration::from_millis(200));

    tokio::time::sleep(Duration::from_millis(1_400)).await;

    let later_health = client
        .request(serde_json::json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
        .await
        .expect("later health request")
        .expect("later health response");
    let later_accepted = later_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    assert_eq!(initial_accepted, later_accepted, "heartbeat should prevent a reconnect");

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn request_succeeds_after_server_side_idle_timeout_reconnect() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    // Server drops the connection quickly; client keep-alive (10s) is far slower than that,
    // so the first request after the pause must go through the reconnect path.
    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    // 2s (not 1s): under a full-workspace `cargo test` the TLS handshake itself can take
    // hundreds of milliseconds, and a 1s idle timeout occasionally killed the connection
    // before `connect` returned.
    options.idle_timeout = Duration::from_secs(2);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let first_health = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("first health request")
        .expect("first health response");
    let first_accepted = first_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    tokio::time::sleep(Duration::from_millis(3_000)).await;

    let second_health = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
        .await
        .expect("request after idle timeout must succeed via reconnect")
        .expect("second health response");
    let second_accepted = second_health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    assert_eq!(second_accepted, first_accepted + 1, "exactly one reconnect expected");

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn concurrent_requests_after_drop_reconnect_only_once() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    options.idle_timeout = Duration::from_secs(1);

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

    tokio::time::sleep(Duration::from_millis(1_800)).await;

    let mut tasks = Vec::new();
    for i in 0..8 {
        let client = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            client
                .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": i}))
                .await
        }));
    }
    for task in tasks {
        let response = task
            .await
            .expect("task join")
            .expect("concurrent request must succeed after reconnect")
            .expect("response");
        assert_eq!(response["result"], json!("pong"));
    }

    let health = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 100}))
        .await
        .expect("health request")
        .expect("health response");
    let accepted = health["result"]["connections"]["accepted"]
        .as_u64()
        .expect("accepted count");

    assert_eq!(
        accepted,
        first_accepted + 1,
        "concurrent failures must share a single reconnect"
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn upstream_login_registers_session_and_resume_works_after_reconnect() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_login_upstream();

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    options.idle_timeout = Duration::from_secs(1);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let login_response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "login",
            "id": 1,
            "params": {
                "phone": "+380991112233",
                "password": "secret",
                "device": {
                    "platform": "android",
                    "appVersion": "1.2.3",
                    "deviceId": "device-123",
                    "model": "Pixel 8",
                    "osVersion": "14"
                }
            }
        }))
        .await
        .expect("login request")
        .expect("login response");

    assert!(login_response.get("error").is_none(), "login failed: {login_response}");
    assert_eq!(login_response["result"]["accessToken"], json!("token-123"));
    let session_id = login_response["result"]["session"]["sessionId"]
        .as_str()
        .expect("server must attach sessionId to upstream login response")
        .to_string();

    // Server-side session must be resumable directly.
    let resume_response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "rpc.resume",
            "id": 2,
            "params": {"sessionId": session_id}
        }))
        .await
        .expect("resume request")
        .expect("resume response");
    assert_eq!(resume_response["result"]["resumed"], json!(true));
    assert_eq!(resume_response["result"]["session"]["accessToken"], json!("token-123"));

    // Let the server drop the connection; the client must reconnect, auto-resume, and succeed.
    tokio::time::sleep(Duration::from_millis(1_800)).await;

    let pong = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.ping", "id": 3}))
        .await
        .expect("request after reconnect")
        .expect("pong");
    assert_eq!(pong["result"], json!("pong"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn logout_closes_connection_and_forgets_session() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_login_upstream();

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();
    let options = base_test_server_options(addr, cert_path.clone(), key_path);
    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let login_response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "login",
            "id": 1,
            "params": {
                "phone": "+380991112233",
                "password": "secret",
                "device": { "deviceId": "device-123" }
            }
        }))
        .await
        .expect("login request")
        .expect("login response");
    let session_id = login_response["result"]["session"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    assert_eq!(client.session_id().await.as_deref(), Some(session_id.as_str()));

    let logout_response = client.logout().await.expect("logout request").expect("logout response");
    assert_eq!(logout_response["result"]["loggedOut"], json!(true));
    assert_eq!(logout_response["result"]["sessionRemoved"], json!(true));
    assert!(
        client.session_id().await.is_none(),
        "logout must forget the session locally"
    );
    assert!(!client.is_connected().await, "logout must close the QUIC connection");

    // The client stays usable (transparent reconnect), but the session is gone on the server.
    let resume = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.resume", "id": 2, "params": {"sessionId": session_id}}))
        .await
        .expect("resume request after logout")
        .expect("resume response");
    assert_eq!(resume["error"]["code"], json!(-32001));
    assert!(client.is_connected().await);

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn server_does_not_drop_silent_connection_before_idle_timeout() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    // No keep-alives from either side would be needed for the connection to survive: both
    // peers advertise long idle timeouts, so silence alone must not end the connection.
    let mut options = base_test_server_options(addr, cert_path.clone(), key_path);
    options.keep_alive_interval = Duration::from_secs(60);
    options.idle_timeout = Duration::from_secs(3600);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let first = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 1}))
        .await
        .expect("health")
        .expect("response");
    let first_accepted = first["result"]["connections"]["accepted"].as_u64().unwrap();

    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(client.is_connected().await);

    let second = client
        .request(json!({"jsonrpc": "2.0", "method": "rpc.health", "id": 2}))
        .await
        .expect("health")
        .expect("response");
    assert_eq!(
        second["result"]["connections"]["accepted"].as_u64().unwrap(),
        first_accepted,
        "no reconnect expected: the server must not drop an idle connection early"
    );

    client.close().await;
    assert!(!client.is_connected().await);

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn start_and_finish_shift_forward_contract_to_upstream() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_upstream();

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let start_response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 1001,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:12Z",
                "sendTime": 1726395072,
                "vehicleId": "vehicle-123",
                "lat": 50.4501,
                "lng": 30.5234
            }
        }))
        .await
        .expect("start request")
        .expect("start response");
    assert_eq!(start_response["result"]["kind"], json!("start"));

    let finish_response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1002,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect("finish request")
        .expect("finish response");
    assert_eq!(finish_response["result"]["kind"], json!("finish"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn finish_shift_without_device_time_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, server) = start_test_server(options).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1003,
            "params": {
                "accessToken": "token-123"
            }
        }))
        .await
        .expect("finishShift request")
        .expect("finishShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn finish_shift_without_access_token_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1004,
            "params": {
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect("finishShift request")
        .expect("finishShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn finish_shift_returns_error_message_from_upstream_http_response() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_finish_shift_error_upstream();

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1005,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect("finishShift request")
        .expect("finishShift response");

    assert_eq!(response["error"]["code"], json!(-32013));
    assert_eq!(response["error"]["message"], json!("Зміну вже завершено"));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn finish_shift_returns_standard_message_for_empty_upstream_error_body() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_finish_shift_empty_error_upstream();

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1006,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect("finishShift request")
        .expect("finishShift response");

    assert_eq!(response["error"]["code"], json!(-32012));
    assert_eq!(
        response["error"]["message"],
        json!("Upstream error response body is empty")
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn finish_shift_returns_transport_error_message_when_upstream_unreachable() {
    let _env_guard = ENV_LOCK.lock().await;
    let unreachable_addr = pick_unused_local_addr();
    let base_url = format!("http://{}", unreachable_addr);

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1007,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect("finishShift request")
        .expect("finishShift response");

    let message = response["error"]["message"].as_str().expect("string error message");
    assert_eq!(response["error"]["code"], json!(-32002));
    assert_ne!(message, "Upstream finishShift request failed");
    assert!(
        message.contains("error")
            || message.contains("connect")
            || message.contains("Connection refused")
            || message.contains("connection refused")
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn request_times_out_when_upstream_is_too_slow() {
    let _env_guard = ENV_LOCK.lock().await;
    let (base_url, upstream) = spawn_mock_slow_finish_shift_upstream(Duration::from_millis(700));

    let _base_url_guard = EnvVarGuard::set("BASE_URL", &base_url);
    let _timeout_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_TIMEOUT_MS", "10000");
    let _retries_guard = EnvVarGuard::set("HTTP_RPC_LOGIN_RETRIES", "0");

    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");
    client.set_request_timeout(Some(Duration::from_millis(200))).await;

    let err = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "finishShift",
            "id": 1008,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:20Z"
            }
        }))
        .await
        .expect_err("request should time out");

    let message = format!("{err:#}");
    assert!(message.contains("timed out"), "expected timeout error, got: {message}");

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
    upstream.join().expect("upstream join");
}

#[tokio::test]
async fn start_shift_without_vehicle_id_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 1008,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:12Z",
                "sendTime": 1726395072
            }
        }))
        .await
        .expect("startShift request")
        .expect("startShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn start_shift_without_send_time_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 1009,
            "params": {
                "accessToken": "token-123",
                "deviceTime": "2026-09-15T10:11:12Z",
                "vehicleId": "vehicle-123"
            }
        }))
        .await
        .expect("startShift request")
        .expect("startShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn start_shift_without_device_time_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 1010,
            "params": {
                "accessToken": "token-123",
                "sendTime": 1726395072,
                "vehicleId": "vehicle-123"
            }
        }))
        .await
        .expect("startShift request")
        .expect("startShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}

#[tokio::test]
async fn start_shift_without_access_token_returns_invalid_params() {
    let (addr, _temp_dir, cert_path, key_path) = create_tls_test_context();

    let options = base_test_server_options(addr, cert_path.clone(), key_path);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { run_with_shutdown(options, shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = QuicJsonRpcClient::connect(addr, "localhost", &cert_path)
        .await
        .expect("client connect");

    let response = client
        .request(json!({
            "jsonrpc": "2.0",
            "method": "startShift",
            "id": 1011,
            "params": {
                "deviceTime": "2026-09-15T10:11:12Z",
                "sendTime": 1726395072,
                "vehicleId": "vehicle-123"
            }
        }))
        .await
        .expect("startShift request")
        .expect("startShift response");

    assert_eq!(response["error"]["code"], json!(-32602));

    let _ = shutdown_tx.send(());
    server.await.expect("server join").expect("server result");
}
