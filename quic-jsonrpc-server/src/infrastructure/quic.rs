//! QUIC transport: accepts connections and bidirectional streams, feeds newline-delimited
//! JSON-RPC lines into the [`RpcDispatcher`] and writes responses back.
//!
//! Connection lifetime policy: the server never closes a client connection because of
//! *application-level* silence. A connection ends only when
//! * the client closes it (app shutdown / `quic_rpc_client_close`),
//! * the client sends `rpc.logout` — the reply is delivered, then the server closes with
//!   [`LOGOUT_CLOSE_CODE`], or
//! * the peer is **unreachable**: no packet of any kind (not even ACKs to our keep-alive PINGs)
//!   has arrived for [`ServerOptions::idle_timeout`] (60 s by default). Both sides send QUIC
//!   keep-alives every [`ServerOptions::keep_alive_interval`], so a live but idle client never
//!   trips this; a phone that vanished without a CONNECTION_CLOSE is reaped instead of staying
//!   half-open for hours. Its session stays resumable for the session TTL.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use anyhow::Result;
use map_tiles::TileService;
use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info};

use crate::{
    application::{AuthService, Metrics, RateLimiter, metrics::ConnectionCloseCause},
    domain::RpcError,
    infrastructure::{config::ServerOptions, logging::sanitize_payload_for_log, tls},
    presentation::{RpcDispatcher, controller::StreamContext, view::JsonRpcView},
};

/// Application close code sent when the server ends a connection because of `rpc.logout`.
pub const LOGOUT_CLOSE_CODE: u32 = 0;
/// Reason phrase accompanying [`LOGOUT_CLOSE_CODE`].
pub const LOGOUT_CLOSE_REASON: &[u8] = b"logout";
/// How long to wait for the client to acknowledge the logout reply before closing anyway.
const LOGOUT_ACK_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum size of one newline-delimited JSON-RPC request. Anything larger is rejected with
/// `-32600` and the stream is reset — otherwise a client could make the server buffer an
/// unbounded "line" in memory.
pub const MAX_REQUEST_LINE_BYTES: usize = 64 * 1024;
/// Rate-limit buckets untouched for this long are dropped.
const RATE_LIMIT_BUCKET_IDLE: Duration = Duration::from_secs(15 * 60);
/// Upper bound on handshakes in flight / connections buffered by the endpoint at once. Protects
/// against connection floods; established connections are bounded by the OS and memory.
pub const MAX_INCOMING_CONNECTIONS: usize = 4096;

/// Tracks active connections and their last activity times for keep-alive detection.
struct ConnectionTracker {
    /// Map of connection ID to last activity Instant
    active_connections: tokio::sync::Mutex<HashMap<u64, Instant>>,
}

impl ConnectionTracker {
    fn new() -> Self {
        Self {
            active_connections: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    async fn record_activity(&self, connection_id: u64) {
        let mut conns = self.active_connections.lock().await;
        conns.insert(connection_id, Instant::now());
    }

    async fn remove(&self, connection_id: u64) {
        let mut conns = self.active_connections.lock().await;
        conns.remove(&connection_id);
    }

    #[allow(dead_code)]
    async fn check_stale(&self, connection_id: u64, keep_alive_interval: Duration) -> bool {
        let conns = self.active_connections.lock().await;
        if let Some(last_activity) = conns.get(&connection_id) {
            last_activity.elapsed() >= keep_alive_interval
        } else {
            false
        }
    }

    async fn count_active(&self) -> usize {
        self.active_connections.lock().await.len()
    }
}

/// Everything the transport needs to serve requests.
pub struct QuicServer {
    pub options: ServerOptions,
    pub dispatcher: Arc<RpcDispatcher>,
    pub metrics: Arc<Metrics>,
    pub auth: Arc<AuthService>,
    pub rate_limiter: Arc<RateLimiter>,
    pub tiles: Arc<TileService>,
}

impl QuicServer {
    pub async fn run(self, mut shutdown: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
        let options = &self.options;
        let server_config = tls::build_server_config(options)?;
        let endpoint = Endpoint::server(server_config, options.bind_addr)?;

        if let Some(paths) = &options.tls_paths {
            info!(
                "TLS mode: file-based cert={} key={} chain={}",
                paths.cert_path.display(),
                paths.key_path.display(),
                paths
                    .chain_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            );
        } else {
            tracing::warn!("TLS mode: ephemeral self-signed certificate");
        }

        info!(
            "JSON-RPC 2.0 over QUIC listening on {} (raw QUIC, not HTTP/3)",
            options.bind_addr
        );
        info!(
            "server options: metrics_interval_secs={} keep_alive_secs={} idle_timeout_secs={} session_ttl_secs={} session_cleanup_interval_secs={} session_debug_enabled={} payload_log_limit={}",
            options.metrics_interval.as_secs(),
            options.keep_alive_interval.as_secs(),
            options.idle_timeout.as_secs(),
            options.session_ttl.as_secs(),
            options.session_cleanup_interval.as_secs(),
            options.session_debug_enabled,
            crate::infrastructure::logging::PAYLOAD_LOG_LIMIT
        );
        info!(
            "limits: max_request_line_bytes={MAX_REQUEST_LINE_BYTES} auth_rate_per_ip={}/min burst={} auth_rate_per_identity={}/min burst={} max_incoming_connections={}",
            options.auth_rate_limit.per_minute,
            options.auth_rate_limit.burst,
            options.auth_identity_rate_limit.per_minute,
            options.auth_identity_rate_limit.burst,
            MAX_INCOMING_CONNECTIONS
        );

        let mut metrics_tick = tokio::time::interval(options.metrics_interval);
        let mut session_cleanup_tick = tokio::time::interval(options.session_cleanup_interval);
        let mut keep_alive_tick = tokio::time::interval(options.keep_alive_interval);
        let connection_tracker = Arc::new(ConnectionTracker::new());

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("shutdown requested, closing endpoint");
                    endpoint.close(VarInt::from_u32(0), b"server shutdown");
                    break;
                }
                _ = metrics_tick.tick() => {
                    info!("metrics: {}", self.metrics.snapshot());
                }
                _ = session_cleanup_tick.tick() => {
                    // A transient Mongo failure must not take the whole transport down — log
                    // and try again on the next tick.
                    match self.auth.cleanup_expired().await {
                        Ok(removed) if removed > 0 => debug!("session cleanup removed {removed} expired session(s)"),
                        Ok(_) => {}
                        Err(err) => tracing::warn!("session cleanup failed: {err:#}"),
                    }
                    let evicted = self.rate_limiter.cleanup(RATE_LIMIT_BUCKET_IDLE);
                    if evicted > 0 {
                        debug!("rate limiter evicted {evicted} idle bucket(s)");
                    }
                    let expired_tiles = self.tiles.evict_expired().await;
                    if expired_tiles > 0 {
                        let (entries, bytes) = self.tiles.cache_usage().await;
                        debug!("tile cache evicted {expired_tiles} expired tile(s); now {entries} tile(s), {bytes} bytes");
                    }
                }
                _ = keep_alive_tick.tick() => {
                    let active = connection_tracker.count_active().await;
                    if active > 0 {
                        debug!("keep-alive checkpoint: {} active connection(s)", active);
                    } else {
                        debug!("keep-alive checkpoint: no active connections");
                    }
                }
                incoming = endpoint.accept() => {
                    let Some(connecting) = incoming else { break };
                    let metrics = Arc::clone(&self.metrics);
                    let dispatcher = Arc::clone(&self.dispatcher);
                    let tracker = Arc::clone(&connection_tracker);
                    tokio::spawn(async move {
                        let handshake_started = Instant::now();
                        match connecting.await {
                            Ok(connection) => {
                                let handshake_elapsed = handshake_started.elapsed();
                                metrics.record_handshake_success(handshake_elapsed);
                                let conn_id = metrics.next_connection_id();
                                metrics.accepted_connections.fetch_add(1, Ordering::Relaxed);
                                debug!(
                                    "connection_id={conn_id} handshake latency={}ms",
                                    handshake_elapsed.as_millis()
                                );
                                if let Err(err) = handle_connection(connection, dispatcher, Arc::clone(&metrics), conn_id, tracker).await {
                                    if is_routine_connection_close(&err) {
                                        debug!("connection_id={conn_id} closed: {err:#}");
                                    } else {
                                        error!("connection_id={conn_id} error: {err:#}");
                                    }
                                }
                                metrics.closed_connections.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(err) => {
                                metrics.record_handshake_failure();
                                tracing::warn!("handshake failed after {}ms: {err:#}", handshake_started.elapsed().as_millis());
                            }
                        }
                    });
                }
            }
        }

        Ok(())
    }
}

async fn handle_connection(
    connection: Connection,
    dispatcher: Arc<RpcDispatcher>,
    metrics: Arc<Metrics>,
    connection_id: u64,
    tracker: Arc<ConnectionTracker>,
) -> Result<()> {
    let remote = connection.remote_address();
    let accepted_at = Instant::now();
    info!("connection_id={connection_id} accepted from {remote}");
    tracker.record_activity(connection_id).await;

    let outcome: Result<()> = loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                tracker.record_activity(connection_id).await;
                let stream_id = metrics.next_stream_id();
                metrics.opened_streams.fetch_add(1, Ordering::Relaxed);
                let metrics = Arc::clone(&metrics);
                let dispatcher = Arc::clone(&dispatcher);
                let connection = connection.clone();
                tokio::spawn(async move {
                    let ctx = StreamContext {
                        connection_id,
                        stream_id,
                        remote_ip: remote.ip(),
                    };
                    debug!("connection_id={connection_id} stream_id={stream_id} opened");
                    if let Err(err) = handle_stream(connection, send, recv, dispatcher, Arc::clone(&metrics), ctx).await
                    {
                        metrics.stream_errors.fetch_add(1, Ordering::Relaxed);
                        error!("connection_id={connection_id} stream_id={stream_id} handling failed: {err:#}");
                    } else {
                        debug!("connection_id={connection_id} stream_id={stream_id} closed");
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_)) => {
                metrics.record_disconnect(ConnectionCloseCause::PeerOrApplicationClosed);
                debug!("connection_id={connection_id} closed by peer/application");
                break Ok(());
            }
            Err(quinn::ConnectionError::LocallyClosed) => {
                metrics.record_disconnect(ConnectionCloseCause::LocallyClosed);
                debug!("connection_id={connection_id} closed locally");
                break Ok(());
            }
            Err(quinn::ConnectionError::TimedOut) => {
                // The peer stopped answering keep-alive PINGs: it is gone without a clean close
                // (dead NAT binding, Doze, network switch). This is the half-open case — log it
                // visibly together with how long the connection had lived.
                metrics.record_disconnect(ConnectionCloseCause::TimedOut);
                let stats = connection.stats();
                info!(
                    "connection_id={connection_id} from {remote} reaped: peer unreachable (idle timeout) age={}s lost_packets={} rtt={}ms",
                    accepted_at.elapsed().as_secs(),
                    stats.path.lost_packets,
                    stats.path.rtt.as_millis()
                );
                break Ok(());
            }
            Err(err) => {
                metrics.record_disconnect(ConnectionCloseCause::TransportError);
                break Err(err.into());
            }
        }
    };

    tracker.remove(connection_id).await;
    info!("connection_id={connection_id} from {remote} closed");
    outcome
}

fn is_routine_connection_close(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<quinn::ConnectionError>(),
        Some(
            quinn::ConnectionError::ApplicationClosed(_)
                | quinn::ConnectionError::LocallyClosed
                | quinn::ConnectionError::TimedOut
        )
    )
}

async fn handle_stream(
    connection: Connection,
    mut send: SendStream,
    recv: RecvStream,
    dispatcher: Arc<RpcDispatcher>,
    metrics: Arc<Metrics>,
    ctx: StreamContext,
) -> Result<()> {
    let StreamContext {
        connection_id,
        stream_id,
        ..
    } = ctx;
    let mut reader = BufReader::new(recv);

    loop {
        let mut line = String::new();
        // `take` caps how much a single `read_line` may pull in; +1 so that a line of exactly
        // MAX bytes plus its newline still fits.
        let bytes = (&mut reader)
            .take(MAX_REQUEST_LINE_BYTES as u64 + 1)
            .read_line(&mut line)
            .await?;
        if bytes == 0 {
            debug!("connection_id={connection_id} stream_id={stream_id} reached EOF");
            break;
        }
        if bytes > MAX_REQUEST_LINE_BYTES || (!line.ends_with('\n') && bytes == MAX_REQUEST_LINE_BYTES + 1) {
            metrics.stream_errors.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                "connection_id={connection_id} stream_id={stream_id} request line exceeds {MAX_REQUEST_LINE_BYTES} bytes; rejecting"
            );
            let response = JsonRpcView::to_line(&JsonRpcView::error(
                serde_json::Value::Null,
                &RpcError::new(crate::domain::codes::INVALID_REQUEST, "Request too large"),
            ));
            let _ = send.write_all(response.as_bytes()).await;
            let _ = send.write_all(b"\n").await;
            let _ = send.finish();
            return Ok(());
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            debug!("connection_id={connection_id} stream_id={stream_id} skipped empty line");
            continue;
        }

        metrics.processed_requests.fetch_add(1, Ordering::Relaxed);
        debug!(
            "connection_id={connection_id} stream_id={stream_id} request_bytes={} payload={}",
            trimmed.len(),
            sanitize_payload_for_log(trimmed)
        );

        let outcome = dispatcher.process_line(trimmed, ctx).await;

        if let Some(response) = &outcome.response {
            send.write_all(response.as_bytes()).await?;
            send.write_all(b"\n").await?;
            send.flush().await?;
            metrics.sent_responses.fetch_add(1, Ordering::Relaxed);
            debug!(
                "connection_id={connection_id} stream_id={stream_id} response_bytes={} payload={}",
                response.len(),
                sanitize_payload_for_log(response)
            );
        } else {
            debug!("connection_id={connection_id} stream_id={stream_id} notification processed without response");
        }

        if outcome.close_connection {
            close_after_logout(&connection, &mut send, connection_id, stream_id).await;
            return Ok(());
        }
    }

    send.finish()?;
    Ok(())
}

/// Finishes the stream, waits (bounded) for the client to acknowledge the logout reply so it is
/// not lost in the CONNECTION_CLOSE race, then closes the connection.
async fn close_after_logout(connection: &Connection, send: &mut SendStream, connection_id: u64, stream_id: u64) {
    let _ = send.finish();
    match tokio::time::timeout(LOGOUT_ACK_TIMEOUT, send.stopped()).await {
        Ok(_) => {}
        Err(_) => debug!(
            "connection_id={connection_id} stream_id={stream_id} logout reply not acknowledged within {:?}; closing anyway",
            LOGOUT_ACK_TIMEOUT
        ),
    }
    info!("connection_id={connection_id} closing connection: client logout");
    connection.close(VarInt::from_u32(LOGOUT_CLOSE_CODE), LOGOUT_CLOSE_REASON);
}
