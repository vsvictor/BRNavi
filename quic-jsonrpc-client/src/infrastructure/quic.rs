//! QUIC connection management: handshake, stale-connection replacement, socket rebind,
//! connection migration and 0‑RTT resumption.
//!
//! Recovery ladder (cheapest first):
//!
//! 1. **Connection migration** — [`QuicTransport::rebind`] swaps the UDP socket under the live
//!    connection (`Endpoint::rebind`). QUIC path validation moves the connection to the new
//!    address/port; no handshake, the session and all streams survive. Used when Android reports
//!    a network change while the connection itself is still open.
//! 2. **0‑RTT reconnect** — if the connection is already closed, a new handshake is started and
//!    the resumption ticket from the previous session is used to send application data in the
//!    very first flight (`Connecting::into_0rtt`). The `rpc.resume` request therefore leaves the
//!    device at time 0 instead of after a full round trip.
//! 3. **1‑RTT reconnect** — fallback when no ticket is available or the server rejects 0‑RTT.
//! 4. **Socket rebind + reconnect** — when even the handshake fails on the current socket (dead
//!    interface), a fresh socket (optionally pinned to a network) is bound and the handshake is
//!    retried on it.

use std::{
    net::{SocketAddr, UdpSocket},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use quinn::{ClientConfig, Connection, Endpoint, EndpointConfig, TokioRuntime, ZeroRttAccepted};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::domain::RequestTimeoutError;

use super::{
    logging::log_response,
    socket::{NetworkHandle, bind_udp_socket, is_socket_path_error},
    tls::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT},
};

/// Binds the endpoint socket, pinned to `network` when possible. A pin that the OS refuses
/// (`android_setsocknetwork` → `ENONET`/`EPERM`/…, typically because the handle is already
/// stale or belongs to a network the app may not use) must not kill the connection attempt:
/// the socket is bound to the default route instead and the *actual* pin is returned so the
/// caller records the truth (`network_handle()` reports it to the Flutter side).
fn bind_with_fallback(
    remote: SocketAddr,
    network: Option<NetworkHandle>,
) -> Result<(UdpSocket, Option<NetworkHandle>)> {
    match bind_udp_socket(remote, network) {
        Ok(socket) => Ok((socket, network.filter(|h| *h != 0))),
        Err(pin_err) if network.is_some_and(|h| h != 0) => {
            eprintln!("binding QUIC socket to network {network:?} failed, using default route: {pin_err:#}");
            let socket = bind_udp_socket(remote, None).with_context(|| format!("pinned attempt: {pin_err:#}"))?;
            Ok((socket, None))
        }
        Err(err) => Err(err),
    }
}

fn new_endpoint(
    client_config: &ClientConfig,
    remote: SocketAddr,
    network: Option<NetworkHandle>,
) -> Result<(Endpoint, Option<NetworkHandle>)> {
    let (socket, bound) = bind_with_fallback(remote, network)?;
    let mut endpoint = Endpoint::new(EndpointConfig::default(), None, socket, Arc::new(TokioRuntime))
        .context("failed to create QUIC endpoint")?;
    endpoint.set_default_client_config(client_config.clone());
    Ok((endpoint, bound))
}

/// Result of a handshake attempt. `zero_rtt` is `Some` when the connection was returned before
/// the handshake finished (early data in flight); the future resolves to whether the server
/// accepted the 0‑RTT data.
struct Handshake {
    connection: Connection,
    zero_rtt: Option<ZeroRttAccepted>,
}

/// Outcome of a successful [`QuicTransport::reconnect_if_stale`].
pub struct Reconnected {
    pub connection: Connection,
    /// `true` when the connection is a 0‑RTT connection: the handshake has not been confirmed
    /// yet and only idempotent data may be sent until [`QuicTransport::await_zero_rtt`] returns.
    pub zero_rtt: bool,
}

async fn handshake(
    endpoint: &Endpoint,
    server_addr: SocketAddr,
    server_name: &str,
    what: &str,
    allow_0rtt: bool,
) -> Result<Handshake> {
    let connecting = endpoint
        .connect(server_addr, server_name)
        .with_context(|| format!("failed to create QUIC {what} request"))?;

    let connecting = if allow_0rtt {
        match connecting.into_0rtt() {
            // A resumption ticket is available: hand the connection out immediately. Only
            // idempotent requests may be sent before `zero_rtt` resolves (see application layer).
            Ok((connection, accepted)) => {
                return Ok(Handshake {
                    connection,
                    zero_rtt: Some(accepted),
                });
            }
            Err(connecting) => connecting,
        }
    } else {
        connecting
    };

    match tokio::time::timeout(DEFAULT_CONNECT_TIMEOUT, connecting).await {
        Ok(result) => result
            .map(|connection| Handshake {
                connection,
                zero_rtt: None,
            })
            .with_context(|| format!("QUIC {what} handshake failed")),
        Err(_) => bail!(
            "QUIC {what} handshake timed out after {}s",
            DEFAULT_CONNECT_TIMEOUT.as_secs()
        ),
    }
}

/// Owns the UDP endpoint and the current QUIC connection.
pub struct QuicTransport {
    /// Kept behind a mutex so that a failed reconnect can rebind a fresh socket (e.g. after the
    /// device switched Wi-Fi <-> mobile data and the old socket is dead).
    endpoint: tokio::sync::Mutex<Endpoint>,
    client_config: ClientConfig,
    connection: tokio::sync::Mutex<Connection>,
    server_addr: SocketAddr,
    server_name: String,
    /// Android network the socket is pinned to (`None` = default route). Updated by `rebind`.
    network: tokio::sync::Mutex<Option<NetworkHandle>>,
    /// Set when the last failure looked like a dead socket; the next reconnect then skips the
    /// doomed attempt on the old socket and rebinds straight away.
    socket_suspect: std::sync::atomic::AtomicBool,
    /// Pending 0‑RTT confirmation for the current connection. While `Some`, the connection is
    /// usable only for idempotent early data; `await_zero_rtt` drains it.
    zero_rtt_gate: tokio::sync::Mutex<Option<ZeroRttAccepted>>,
}

impl QuicTransport {
    pub async fn connect(server_addr: SocketAddr, server_name: &str, client_config: ClientConfig) -> Result<Self> {
        Self::connect_on_network(server_addr, server_name, client_config, None).await
    }

    /// Like [`connect`](Self::connect) but pins the UDP socket to an Android network handle.
    pub async fn connect_on_network(
        server_addr: SocketAddr,
        server_name: &str,
        client_config: ClientConfig,
        network: Option<NetworkHandle>,
    ) -> Result<Self> {
        let (endpoint, bound) = new_endpoint(&client_config, server_addr, network)?;
        // The initial connect always waits for the full handshake: there is nothing to send yet
        // and the caller needs a definitive success/failure.
        let Handshake { connection, .. } = handshake(&endpoint, server_addr, server_name, "connect", false).await?;
        Ok(Self {
            endpoint: tokio::sync::Mutex::new(endpoint),
            client_config,
            connection: tokio::sync::Mutex::new(connection),
            server_addr,
            server_name: server_name.to_string(),
            network: tokio::sync::Mutex::new(bound),
            socket_suspect: std::sync::atomic::AtomicBool::new(false),
            zero_rtt_gate: tokio::sync::Mutex::new(None),
        })
    }

    /// Current connection **without** waiting for a pending 0‑RTT confirmation. Only for
    /// idempotent requests (`rpc.ping`, `rpc.resume`) that are allowed to be replayed.
    pub async fn current(&self) -> Connection {
        self.connection.lock().await.clone()
    }

    /// Current connection, guaranteed to have a completed handshake. If a 0‑RTT reconnect is in
    /// flight this waits (≤ one RTT, bounded) for the server to confirm it, so non-idempotent
    /// application data is never sent as replayable early data.
    pub async fn current_confirmed(&self) -> Result<Connection> {
        self.await_zero_rtt().await?;
        Ok(self.current().await)
    }

    /// Waits for the pending 0‑RTT confirmation, if any. Returns `Some(accepted)` when there
    /// was one, `None` when the current connection was established with a full handshake.
    /// Concurrent callers serialize on the gate, so each of them observes the result.
    pub async fn await_zero_rtt(&self) -> Result<Option<bool>> {
        let mut gate = self.zero_rtt_gate.lock().await;
        let Some(pending) = gate.take() else {
            return Ok(None);
        };
        match tokio::time::timeout(DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT, pending).await {
            Ok(accepted) => Ok(Some(accepted)),
            Err(_) => bail!(
                "0-RTT handshake confirmation timed out after {}s",
                DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT.as_secs()
            ),
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.connection.lock().await.close_reason().is_none()
    }

    /// Network handle the socket is currently bound to (`None` = default route).
    pub async fn network(&self) -> Option<NetworkHandle> {
        *self.network.lock().await
    }

    /// Records that `err` came from a request on the current socket. Socket-path errors
    /// (`ENETUNREACH`, `ECONNRESET`, …) mark the socket as suspect so the next reconnect rebinds
    /// immediately instead of first burning a connect timeout on the dead socket.
    pub fn note_request_error(&self, err: &anyhow::Error) {
        if is_socket_path_error(err) {
            self.mark_socket_suspect();
        }
    }

    /// Marks the current socket as dead so the next reconnect rebinds immediately.
    pub fn mark_socket_suspect(&self) {
        self.socket_suspect.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Closes the current connection with an application close code and waits (bounded) until
    /// the CONNECTION_CLOSE frame has actually been flushed, so the server sees a clean close
    /// instead of having to wait for its idle timeout.
    pub async fn close(&self, code: u32, reason: &[u8], drain_timeout: Duration) {
        let connection = self.connection.lock().await.clone();
        connection.close(code.into(), reason);
        let endpoint = self.endpoint.lock().await.clone();
        let _ = tokio::time::timeout(drain_timeout, endpoint.wait_idle()).await;
    }

    /// **Connection migration.** Binds a fresh UDP socket (optionally pinned to `network`) and
    /// swaps it under the endpoint. The open QUIC connection migrates to the new local
    /// address/port via path validation — no handshake, no session loss. Returns `true` when the
    /// connection was still open and therefore migrated; `false` when it was already closed and
    /// the caller must run a reconnect (which will now use the new socket).
    pub async fn rebind(&self, network: Option<NetworkHandle>) -> Result<bool> {
        // Same lock order as `reconnect_if_stale` (connection → endpoint → network) so a rebind
        // racing a reconnect cannot deadlock.
        let connection = self.connection.lock().await;
        let endpoint = self.endpoint.lock().await;
        // An explicit rebind must land on the requested network or fail loudly: the caller
        // chose that network for a reason and will pick another one on error. Silently moving
        // to the default route here would only hide a dead/forbidden handle.
        let socket = bind_udp_socket(self.server_addr, network)?;
        endpoint
            .rebind(socket)
            .context("failed to rebind QUIC endpoint socket")?;
        *self.network.lock().await = network.filter(|h| *h != 0);
        self.socket_suspect.store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(connection.close_reason().is_none())
    }

    /// Binds a fresh socket (optionally pinned, falling back to the default route when the OS
    /// refuses the pin) and runs a **full** handshake on it. 0‑RTT is deliberately not used
    /// here: on a freshly bound socket we have no evidence the path works, and a 0‑RTT
    /// "success" would only surface as a dead connection one confirmation later.
    async fn handshake_on_fresh_socket(
        &self,
        network: Option<NetworkHandle>,
    ) -> Result<(Endpoint, Option<NetworkHandle>, Handshake)> {
        let (fresh, bound) = new_endpoint(&self.client_config, self.server_addr, network)?;
        let handshake = handshake(&fresh, self.server_addr, &self.server_name, "reconnect", false).await?;
        Ok((fresh, bound, handshake))
    }

    /// Re-establishes the QUIC connection unless another task already replaced the connection
    /// that `failed` refers to. Returns `Some(reconnected)` when a new connection was created by
    /// this call, `None` otherwise.
    ///
    /// The handshake is first attempted on the existing endpoint (skipped when the socket is
    /// suspect); if that fails the UDP socket is rebound (new `Endpoint`, same network pin) and
    /// the handshake is retried once. A stale socket is the typical failure mode after the device
    /// changed networks.
    pub async fn reconnect_if_stale(&self, failed: &Connection) -> Result<Option<Reconnected>> {
        // Hold the lock across the handshake so concurrent callers serialize on it and observe
        // the fresh connection instead of racing to create their own.
        let mut guard = self.connection.lock().await;

        let already_replaced = guard.stable_id() != failed.stable_id() && guard.close_reason().is_none();
        if already_replaced {
            return Ok(None);
        }

        let mut endpoint_guard = self.endpoint.lock().await;
        let network = *self.network.lock().await;
        let suspect = self.socket_suspect.swap(false, std::sync::atomic::Ordering::Relaxed);

        let first_attempt = if suspect {
            Err(anyhow::anyhow!("socket marked dead by previous request error"))
        } else {
            handshake(&endpoint_guard, self.server_addr, &self.server_name, "reconnect", true).await
        };

        let Handshake { connection, zero_rtt } = match first_attempt {
            Ok(handshake) => handshake,
            Err(first_err) => {
                eprintln!("reconnect on existing socket failed, rebinding endpoint: {first_err:#}");
                let (fresh, bound, handshake) = match self.handshake_on_fresh_socket(network).await {
                    Ok(ok) => ok,
                    // The pinned network may itself be gone (Wi‑Fi turned off while the socket was
                    // pinned to it): a socket bound to a dead network black-holes forever. Fall
                    // back to the default route once and forget the stale pin.
                    Err(pinned_err) if network.is_some() => {
                        eprintln!(
                            "reconnect on pinned network {network:?} failed, falling back to default route: {pinned_err:#}"
                        );
                        self.handshake_on_fresh_socket(None)
                            .await
                            .with_context(|| format!("pinned attempt: {pinned_err:#}"))
                            .with_context(|| format!("first attempt: {first_err:#}"))?
                    }
                    Err(err) => return Err(err.context(format!("first attempt: {first_err:#}"))),
                };
                *self.network.lock().await = bound;
                endpoint_guard.close(0u32.into(), b"rebind");
                *endpoint_guard = fresh;
                handshake
            }
        };

        let is_zero_rtt = zero_rtt.is_some();
        // Publish the gate before the connection so no reader can see an unconfirmed connection
        // without a pending gate.
        *self.zero_rtt_gate.lock().await = zero_rtt;
        *guard = connection.clone();
        Ok(Some(Reconnected {
            connection,
            zero_rtt: is_zero_rtt,
        }))
    }
}

/// One request/response exchange on a fresh bidirectional stream (newline-delimited JSON).
pub async fn send_request_on_connection(
    connection: &Connection,
    request: Value,
    timeout: Option<Duration>,
) -> Result<Option<Value>> {
    let request_fut = async {
        let (mut send, recv) = connection.open_bi().await?;
        let payload = request.to_string();
        send.write_all(payload.as_bytes()).await?;
        send.write_all(b"\n").await?;
        send.finish()?;

        let mut reader = BufReader::new(recv);
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            log_response(&request, None, "empty");
            return Ok(None);
        }

        let parsed = serde_json::from_str::<Value>(line.trim())
            .inspect_err(|_| log_response(&request, None, "non-JSON response (payload withheld)"))
            .context("server returned non-JSON response")?;
        log_response(&request, Some(&parsed), "received");
        Ok(Some(parsed))
    };

    if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, request_fut).await {
            Ok(result) => result,
            Err(_) => Err(RequestTimeoutError(timeout).into()),
        }
    } else {
        request_fut.await
    }
}
