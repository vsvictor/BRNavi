use std::{
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use quinn::Connection;
use serde_json::{Value, json};

use crate::{
    domain::{
        JsonRpcRequest, RequestTimeoutError, TileImage, extract_session_id, extract_tile, is_ping_pong_response,
        is_session_not_found,
    },
    infrastructure::{
        NetworkHandle, QuicTransport, Reconnected, build_client_config, is_socket_path_error,
        send_request_on_connection,
    },
};

pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS);
/// Upper bound for the path-validation ping after a connection migration ([`QuicJsonRpcClient::rebind`]).
pub const REBIND_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Application close code the client uses when it ends the connection itself (logout / app exit).
pub const CLIENT_CLOSE_CODE: u32 = 0;
/// How long `close`/`logout` wait for the CONNECTION_CLOSE frame to be flushed.
pub const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
/// Consecutive heartbeat failures after which the path is considered dead and the client
/// reconnects. With a 24h idle timeout the QUIC layer will not notice a dead path on its own.
pub const HEARTBEAT_FAILURES_BEFORE_RECONNECT: u32 = 2;
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;
pub const RECONNECT_BACKOFF_BASE: Duration = Duration::from_millis(250);
pub const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Options for [`QuicJsonRpcClient::connect_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectOptions {
    /// Android `Network.getNetworkHandle()` to pin the UDP socket to (`None`/`0` = default route).
    pub network_handle: Option<NetworkHandle>,
}

/// Outcome of [`QuicJsonRpcClient::rebind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebindOutcome {
    /// The live connection migrated to the new socket; no handshake was needed.
    Migrated,
    /// The connection was already dead; a new session was established on the new socket.
    Reconnected,
}

impl RebindOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Migrated => "migrated",
            Self::Reconnected => "reconnected",
        }
    }
}

/// Why `rpc.resume` after a reconnect did not succeed.
enum ResumeError {
    /// The QUIC handshake behind a 0‑RTT connection was never confirmed: the transport is dead.
    HandshakeUnconfirmed(anyhow::Error),
    /// Transport is fine, only the session could not be resumed (expired, unknown, …).
    Resume(anyhow::Error),
}

fn heartbeat_log(event: &str, data: Value) {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    eprintln!(
        "{}",
        json!({
            "component": "quic-jsonrpc-client",
            "stream": "heartbeat",
            "tsMs": ts_ms,
            "event": event,
            "data": data,
        })
    );
}

pub struct QuicJsonRpcClient {
    transport: QuicTransport,
    session_id: tokio::sync::RwLock<Option<String>>,
    request_timeout: tokio::sync::RwLock<Option<Duration>>,
}

pub struct HeartbeatHandle {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HeartbeatHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl QuicJsonRpcClient {
    pub async fn connect(server_addr: SocketAddr, server_name: &str, ca_cert_path: &Path) -> Result<Self> {
        Self::connect_with_options(server_addr, server_name, ca_cert_path, ConnectOptions::default()).await
    }

    /// Connects with explicit options (e.g. pinning the UDP socket to an Android network).
    pub async fn connect_with_options(
        server_addr: SocketAddr,
        server_name: &str,
        ca_cert_path: &Path,
        options: ConnectOptions,
    ) -> Result<Self> {
        let client_config = build_client_config(ca_cert_path)?;
        let transport =
            QuicTransport::connect_on_network(server_addr, server_name, client_config, options.network_handle).await?;
        Ok(Self {
            transport,
            session_id: tokio::sync::RwLock::new(None),
            request_timeout: tokio::sync::RwLock::new(Some(DEFAULT_REQUEST_TIMEOUT)),
        })
    }

    /// `true` while the current QUIC connection has not been closed by either side.
    pub async fn is_connected(&self) -> bool {
        self.transport.is_connected().await
    }

    /// Liveness probe with its **own** timeout, independent of the client-wide request timeout.
    ///
    /// Sends `rpc.ping` on the current connection and returns `Ok(())` on a well-formed pong.
    /// Unlike [`Self::request`] it never reconnects or retries: it is meant for the app-side
    /// pulse, where a black-holed path must surface after a few seconds instead of blocking the
    /// (serialised) native call queue for the full request timeout. `rpc.ping` is idempotent, so
    /// an unconfirmed (0‑RTT) connection is acceptable here.
    pub async fn ping(&self, timeout: Duration) -> Result<()> {
        let connection = self.transport.current().await;
        if connection.close_reason().is_some() {
            bail!("ping: connection is closed");
        }
        let response = send_request_on_connection(&connection, JsonRpcRequest::ping("pulse"), Some(timeout)).await?;
        if is_ping_pong_response(response.as_ref()) {
            Ok(())
        } else {
            bail!("ping: unexpected response {response:?}")
        }
    }

    /// Marks the current UDP socket as dead (e.g. the OS reported that the network it is pinned
    /// to was lost). The next reconnect then binds a fresh socket immediately instead of first
    /// burning a connect timeout on the dead one. Cheap, no I/O.
    pub fn mark_socket_suspect(&self) {
        self.transport.mark_socket_suspect();
    }

    /// Network handle the socket is pinned to (`None` = default route).
    pub async fn network_handle(&self) -> Option<NetworkHandle> {
        self.transport.network().await
    }

    /// Forces a fresh QUIC connection (and, if needed, a fresh UDP socket) and re-attaches the
    /// session. Returns an error only when the transport could not be re-established.
    pub async fn reconnect(&self) -> Result<()> {
        self.reconnect_with_retry("explicit_reconnect").await
    }

    /// Reacts to a network change reported by the OS. Binds a new UDP socket (pinned to
    /// `network_handle` when given) and:
    ///
    /// * if the QUIC connection is still open → **migrates** it (path validation only, the
    ///   session and streams survive, nothing is re-sent);
    /// * otherwise → reconnects on the new socket, using 0‑RTT when a ticket is available.
    ///
    /// A short validation ping confirms the migrated path really works; if it does not, the
    /// client falls through to a reconnect so the caller never ends up with a half-dead
    /// connection.
    pub async fn rebind(&self, network_handle: Option<NetworkHandle>) -> Result<RebindOutcome> {
        let request_timeout = *self.request_timeout.read().await;
        let migrated = self.transport.rebind(network_handle).await?;
        if migrated {
            // Ping is idempotent, so the unconfirmed connection is fine here.
            let connection = self.transport.current().await;
            // Bound the validation so a black-holed path cannot stall the caller. Mobile RTTs are
            // well under a second, so a short cap keeps a failed migration from delaying the
            // reconnect fallback; the request timeout may be disabled by the app, hence the
            // explicit fallback.
            let probe_timeout = Some(
                request_timeout
                    .unwrap_or(REBIND_PROBE_TIMEOUT)
                    .min(REBIND_PROBE_TIMEOUT),
            );
            match send_request_on_connection(&connection, JsonRpcRequest::ping("rebind"), probe_timeout).await {
                Ok(response) if is_ping_pong_response(response.as_ref()) => return Ok(RebindOutcome::Migrated),
                Ok(_) | Err(_) => {
                    heartbeat_log("migration_probe_failed", json!({ "networkHandle": network_handle }));
                }
            }
        }
        self.reconnect_with_retry("rebind").await?;
        Ok(RebindOutcome::Reconnected)
    }

    pub async fn set_request_timeout(&self, timeout: Option<Duration>) {
        *self.request_timeout.write().await = timeout;
    }

    pub async fn request(&self, request: Value) -> Result<Option<Value>> {
        let mut attempts = 0;
        loop {
            // Application requests are not necessarily idempotent → never send them as 0‑RTT
            // early data; wait for the handshake confirmation if one is pending (≤ 1 RTT).
            let connection = self.transport.current_confirmed().await?;
            let request_timeout = *self.request_timeout.read().await;

            match send_request_on_connection(&connection, request.clone(), request_timeout).await {
                Ok(response) => {
                    self.capture_session_id(&response).await;
                    return Ok(response);
                }
                Err(err) if attempts == 0 && should_reconnect_after(&err, &connection) => {
                    attempts += 1;
                    // `ENETUNREACH` / `ECONNRESET` mean the socket itself is dead (interface
                    // gone): tell the transport so the reconnect rebinds immediately instead of
                    // first waiting out a connect timeout on the dead socket.
                    self.transport.note_request_error(&err);
                    self.reconnect_with_retry("request_recovery")
                        .await
                        .with_context(|| format!("request failed ({err:#}) and reconnect failed"))?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn reconnect_with_retry(&self, reason: &str) -> Result<()> {
        let request_timeout = *self.request_timeout.read().await;
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
            let current = self.transport.current().await;
            // Mark the current connection as failed so `reconnect_if_stale` does not treat it as
            // already replaced and skip the handshake.
            current.close(0u32.into(), b"client reconnect");

            match self.transport.reconnect_if_stale(&current).await {
                Ok(Some(reconnected)) => {
                    let zero_rtt = reconnected.zero_rtt;
                    let connection = reconnected.connection.clone();
                    match self.resume_after_reconnect(reconnected, request_timeout).await {
                        Ok(()) => {}
                        // The handshake itself never completed: the new connection is as dead as
                        // the old one (typical for a socket pinned to a network that just went
                        // away). This is a *failed* attempt, not a resume hiccup.
                        Err(ResumeError::HandshakeUnconfirmed(err)) => {
                            connection.close(0u32.into(), b"0-rtt unconfirmed");
                            let wrapped = err.context(format!(
                                "{reason}: reconnect attempt {attempt}/{MAX_RECONNECT_ATTEMPTS} did not complete the handshake"
                            ));
                            eprintln!("{wrapped:#}");
                            self.transport.mark_socket_suspect();
                            last_error = Some(wrapped);
                            continue;
                        }
                        // Session resume is best-effort: transport health is primary.
                        Err(ResumeError::Resume(resume_err)) => {
                            eprintln!("session resume after reconnect failed: {resume_err:#}");
                        }
                    }
                    heartbeat_log(
                        "reconnect_done",
                        json!({ "reason": reason, "attempt": attempt, "zeroRtt": zero_rtt }),
                    );
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Err(err) => {
                    let socket_dead = is_socket_path_error(&err);
                    let wrapped = err.context(format!(
                        "{reason}: reconnect attempt {attempt}/{MAX_RECONNECT_ATTEMPTS} failed"
                    ));
                    eprintln!("{wrapped:#}");
                    last_error = Some(wrapped);
                    if attempt < MAX_RECONNECT_ATTEMPTS {
                        // A dead socket is not a congestion signal — retry at once on a fresh one.
                        if socket_dead {
                            self.transport
                                .note_request_error(last_error.as_ref().expect("just set"));
                        } else {
                            tokio::time::sleep(reconnect_backoff(attempt)).await;
                        }
                    }
                }
            }
        }

        if let Some(err) = last_error {
            Err(err)
        } else {
            bail!("{reason}: reconnect failed")
        }
    }

    /// Runs `rpc.resume` on a freshly established connection.
    ///
    /// With 0‑RTT the resume request is written into the first flight, *before* the handshake
    /// completes. `rpc.resume` is idempotent, so replay is harmless. The 0‑RTT confirmation is
    /// then awaited (this also opens the gate for regular requests): if the server rejected early
    /// data the request was silently dropped and is re-sent on the now-complete 1‑RTT connection.
    async fn resume_after_reconnect(
        &self,
        reconnected: Reconnected,
        request_timeout: Option<Duration>,
    ) -> std::result::Result<(), ResumeError> {
        let Reconnected { connection, zero_rtt } = reconnected;
        if !zero_rtt {
            return self
                .resume_on(&connection, request_timeout)
                .await
                .map_err(ResumeError::Resume);
        }

        let early = self.resume_on(&connection, request_timeout).await;
        let accepted = self
            .transport
            .await_zero_rtt()
            .await
            .map_err(ResumeError::HandshakeUnconfirmed)?
            .unwrap_or(true);
        heartbeat_log(
            "zero_rtt",
            json!({ "accepted": accepted, "earlyResumeOk": early.is_ok() }),
        );
        if accepted {
            return early.map_err(ResumeError::Resume);
        }
        // Early data rejected: the first resume never reached the server.
        self.resume_on(&connection, request_timeout)
            .await
            .map_err(ResumeError::Resume)
    }

    async fn resume_on(&self, connection: &Connection, request_timeout: Option<Duration>) -> Result<()> {
        let Some(session_id) = self.session_id.read().await.clone() else {
            return Ok(());
        };

        let response =
            send_request_on_connection(connection, JsonRpcRequest::resume(&session_id), request_timeout).await?;
        match response {
            Some(payload) if payload.get("error").is_none() => Ok(()),
            Some(payload) => {
                if is_session_not_found(&payload) {
                    // The server no longer knows this session; stop trying to resume it on
                    // every reconnect. A fresh login will capture a new id.
                    let mut guard = self.session_id.write().await;
                    if guard.as_deref() == Some(session_id.as_str()) {
                        *guard = None;
                    }
                }
                bail!("session resume failed: {payload}")
            }
            None => bail!("session resume failed: empty response"),
        }
    }

    async fn capture_session_id(&self, response: &Option<Value>) {
        if let Some(session_id) = response.as_ref().and_then(extract_session_id) {
            *self.session_id.write().await = Some(session_id.to_string());
        }
    }

    pub fn spawn_heartbeat(self: Arc<Self>, interval: Duration) -> HeartbeatHandle {
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            let mut consecutive_failures = 0u32;
            loop {
                ticker.tick().await;
                let sent_at = std::time::Instant::now();
                heartbeat_log(
                    "ping_send",
                    json!({
                        "intervalMs": interval.as_millis(),
                        "consecutiveFailures": consecutive_failures,
                    }),
                );
                match self.request(JsonRpcRequest::ping("heartbeat")).await {
                    Ok(response) if is_ping_pong_response(response.as_ref()) => {
                        heartbeat_log(
                            "pong_ok",
                            json!({
                                "latencyMs": sent_at.elapsed().as_millis(),
                            }),
                        );
                        consecutive_failures = 0;
                    }
                    Ok(response) => {
                        consecutive_failures += 1;
                        heartbeat_log(
                            "pong_invalid",
                            json!({
                                "latencyMs": sent_at.elapsed().as_millis(),
                                "consecutiveFailures": consecutive_failures,
                                "response": response,
                            }),
                        );
                        eprintln!(
                            "heartbeat ping returned non-pong ({consecutive_failures}): {:?}",
                            response
                        );
                        if consecutive_failures >= HEARTBEAT_FAILURES_BEFORE_RECONNECT {
                            heartbeat_log(
                                "reconnect_start",
                                json!({
                                    "reason": "invalid_pong",
                                    "consecutiveFailures": consecutive_failures,
                                }),
                            );
                            match self.reconnect().await {
                                Ok(()) => {
                                    heartbeat_log(
                                        "reconnect_ok",
                                        json!({
                                            "reason": "invalid_pong",
                                        }),
                                    );
                                    consecutive_failures = 0;
                                }
                                Err(reconnect_err) => {
                                    heartbeat_log(
                                        "reconnect_failed",
                                        json!({
                                            "reason": "invalid_pong",
                                            "error": reconnect_err.to_string(),
                                        }),
                                    );
                                    eprintln!("heartbeat-triggered reconnect failed: {reconnect_err:#}")
                                }
                            }
                        }
                    }
                    Err(err) => {
                        consecutive_failures += 1;
                        heartbeat_log(
                            "ping_failed",
                            json!({
                                "latencyMs": sent_at.elapsed().as_millis(),
                                "consecutiveFailures": consecutive_failures,
                                "error": err.to_string(),
                            }),
                        );
                        eprintln!("heartbeat ping failed ({consecutive_failures}): {err:#}");
                        if consecutive_failures >= HEARTBEAT_FAILURES_BEFORE_RECONNECT {
                            // The QUIC idle timeout is 24h, so a dead path (network switch,
                            // NAT rebinding, ...) has to be detected here.
                            heartbeat_log(
                                "reconnect_start",
                                json!({
                                    "reason": "request_error",
                                    "consecutiveFailures": consecutive_failures,
                                }),
                            );
                            match self.reconnect().await {
                                Ok(()) => {
                                    heartbeat_log(
                                        "reconnect_ok",
                                        json!({
                                            "reason": "request_error",
                                        }),
                                    );
                                    consecutive_failures = 0;
                                }
                                Err(reconnect_err) => {
                                    heartbeat_log(
                                        "reconnect_failed",
                                        json!({
                                            "reason": "request_error",
                                            "error": reconnect_err.to_string(),
                                        }),
                                    );
                                    eprintln!("heartbeat-triggered reconnect failed: {reconnect_err:#}")
                                }
                            }
                        }
                    }
                }
            }
        });
        HeartbeatHandle { task }
    }

    /// Session id captured from the last successful login / resume, if any.
    pub async fn session_id(&self) -> Option<String> {
        self.session_id.read().await.clone()
    }

    /// Fetches one map tile via `map.getTile` (server-side OSM proxy + cache) and decodes the
    /// base64 payload. A JSON-RPC error from the server is returned as an `Err` carrying its
    /// `code` / `message`.
    pub async fn get_tile(&self, z: u8, x: u32, y: u32) -> Result<TileImage> {
        let response = self
            .request(JsonRpcRequest::get_tile(z, x, y))
            .await?
            .ok_or_else(|| anyhow::anyhow!("map.getTile returned no response"))?;
        extract_tile(&response).map_err(|error| {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            anyhow::anyhow!("map.getTile {z}/{x}/{y} failed: [{code}] {message}")
        })
    }

    /// Explicit logout: tells the server to forget the session (`rpc.logout`) and closes the
    /// QUIC connection. Apart from this and a client-side `close`, the server only ends a
    /// connection when the peer becomes unreachable (QUIC idle timeout).
    ///
    /// Best-effort: if the transport is already dead the local state is still cleared and the
    /// connection closed. The client object stays usable — the next `request` reconnects.
    pub async fn logout(&self) -> Result<Option<Value>> {
        let session_id = self.session_id.write().await.take();
        let request_timeout = *self.request_timeout.read().await;

        // Prefer a confirmed connection so the logout is neither replayed nor silently dropped
        // as rejected early data; if confirmation fails, fall back to whatever we have.
        let connection = match self.transport.current_confirmed().await {
            Ok(connection) => connection,
            Err(_) => self.transport.current().await,
        };
        let result = if connection.close_reason().is_none() {
            send_request_on_connection(
                &connection,
                JsonRpcRequest::logout(session_id.as_deref()),
                request_timeout,
            )
            .await
        } else {
            Ok(None)
        };

        self.transport
            .close(CLIENT_CLOSE_CODE, b"logout", CLOSE_DRAIN_TIMEOUT)
            .await;
        result
    }

    /// Gracefully closes the QUIC connection without touching the server-side session (the
    /// session stays resumable). Used when the app shuts down.
    pub async fn close(&self) {
        self.transport
            .close(CLIENT_CLOSE_CODE, b"client closed", CLOSE_DRAIN_TIMEOUT)
            .await;
    }
}

fn reconnect_backoff(attempt: u32) -> Duration {
    let shift = (attempt.saturating_sub(1)).min(5);
    let factor = 1u32 << shift;
    let step = RECONNECT_BACKOFF_BASE.mul_f32(factor as f32);
    if step > RECONNECT_BACKOFF_MAX {
        RECONNECT_BACKOFF_MAX
    } else {
        step
    }
}

/// Decides whether a failed request should trigger a transparent reconnect + retry.
///
/// Transport errors (stream/connection closed, reset, ...) always do. A request *timeout* only
/// does when the connection is already known to be closed: otherwise the server may simply be
/// slow and a blind retry would double the latency.
fn should_reconnect_after(err: &anyhow::Error, connection: &Connection) -> bool {
    if err.downcast_ref::<RequestTimeoutError>().is_none() {
        return true;
    }
    connection.close_reason().is_some()
}
