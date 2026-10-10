Optional heartbeat loop:

```bash
QUIC_RPC_HEARTBEAT_SECS=10 cargo run -p quic-jsonrpc-client -- 127.0.0.1:8443 ./quic-jsonrpc-client/certs/ca-root.pem '{"jsonrpc":"2.0","method":"login","id":1,"params":{"phone":"+380991112233","password":"secret","device":{"platform":"android","appVersion":"1.2.3","deviceId":"device-123","model":"Pixel 8","osVersion":"14"}}}' --server-name mobilespace.app
```
When `QUIC_RPC_HEARTBEAT_SECS` is set, the CLI keeps the process alive after the request and sends periodic `rpc.ping` frames to prevent idle timeout.

Request timeout support:

- Default: `5000 ms`
- Code constants: `DEFAULT_REQUEST_TIMEOUT_MS` and `DEFAULT_REQUEST_TIMEOUT`
- CLI flag: `--timeout-ms <millis>`
- Environment variable: `QUIC_RPC_REQUEST_TIMEOUT_MS`
- Native API: `quic_rpc_client_set_request_timeout(client, timeout_ms)`

Set the timeout value to `0` to disable it.

The timeout applies to each RPC request, including reconnect/resume attempts and the heartbeat ping loop.

## Connection lifetime

- The server closes a QUIC connection only on `rpc.logout` or when the peer is **unreachable**: nothing (not even ACKs to keep-alive PINGs) received for `QUIC_RPC_IDLE_TIMEOUT_SECS` (60 s). Both sides send keep-alives every 10 s, so an idle-but-live client is never dropped; a vanished phone no longer lingers as a half-open connection.
- QUIC uses the *minimum* of both peers' idle timeouts, so the client advertises 60 s as well (`DEFAULT_MAX_IDLE_TIMEOUT`). A dead network path is detected by the heartbeat (after `HEARTBEAT_FAILURES_BEFORE_RECONNECT` (2) consecutive ping failures) or, at the latest, by the idle timeout; the client then reconnects (0‑RTT) and resumes the session, which lives server-side for `QUIC_RPC_SESSION_TTL_SECS` (24 h).
- The client ends the connection only itself:
  - `QuicJsonRpcClient::logout()` / `quic_rpc_client_logout(client)` — sends `rpc.logout {sessionId}`, the server deletes the session, delivers the reply and closes the connection. The handle stays usable (next request reconnects without a session).
  - `QuicJsonRpcClient::close()` / `quic_rpc_client_close(client)` — graceful CONNECTION_CLOSE, server-side session stays resumable.

## Network changes (Android): migration, socket binding, 0‑RTT

When Android changes the default network (USB cable unplugged → `rndis0`/`usb0` gone, Wi‑Fi ↔ cellular,
VPN up/down) the kernel invalidates UDP sockets bound to the default route: sends fail with
`ENETUNREACH`/`ECONNRESET` and inbound packets are dropped. The client recovers in this order
(cheapest first):

1. **Connection migration** — `QuicJsonRpcClient::rebind(network_handle)` /
   `quic_rpc_client_rebind(client, network_handle)` binds a fresh UDP socket and swaps it under the live
   connection (`Endpoint::rebind`). QUIC path validation moves the connection to the new address/port;
   **no handshake, the session and streams survive**. A short `rpc.ping` (`REBIND_PROBE_TIMEOUT`, 3 s)
   validates the new path.
   The server accepts this because `migration(true)` is set in `quic-jsonrpc-server/src/infrastructure/tls.rs`
   (quinn default, made explicit). Returns `{"ok":true,"response":{"outcome":"migrated"}}`.
2. **0‑RTT reconnect** — if the connection is already dead, a new handshake is started and the TLS
   resumption ticket from the previous session is used (`Connecting::into_0rtt`): `rpc.resume` is written
   into the very first flight, before the handshake completes. Only idempotent requests (`rpc.resume`,
   `rpc.ping`) are sent as early data; everything else waits for the `ZeroRttAccepted` confirmation
   (≤ 1 RTT, bounded by `DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT`). If the server rejects early data the resume
   is re-sent on the 1‑RTT connection. Outcome: `"reconnected"`.
   * Session tickets are kept in a **process-wide** `ClientConfig` cache (`build_client_config`), so they
     survive `quic_rpc_client_close` → `quic_rpc_client_connect` cycles driven from Dart.
   * Server side: `max_early_data_size = u32::MAX`, `ServerSessionMemoryCache(16 384)`,
     4 tickets per handshake (`quic-jsonrpc-server/src/infrastructure/tls.rs`).
3. **1‑RTT reconnect** — fallback when no ticket exists.
4. **Socket rebind + reconnect** — when even the handshake fails on the current socket, a fresh socket is
   bound and the handshake retried. Socket-path errors (`ENETUNREACH`, `ENETDOWN`, `EHOSTUNREACH`,
   `ECONNRESET`, …, see `is_socket_path_error`) mark the socket as *suspect* so the next reconnect skips
   the doomed attempt on the dead socket and rebinds immediately, without back-off.

**Socket binding to a specific network.** `quic_rpc_client_connect_on_network(addr, name, ca, network_handle)`
and `quic_rpc_client_rebind(client, network_handle)` take an Android `Network.getNetworkHandle()` (API 23+)
and pin the UDP socket to that network via `android_setsocknetwork` (resolved at runtime from
`libandroid.so`). `network_handle == 0` means "default route" and behaves exactly like the old
`quic_rpc_client_connect`. On non-Android platforms the handle is ignored.

Rust API: `QuicJsonRpcClient::connect_with_options(addr, name, ca, ConnectOptions { network_handle })`,
`QuicJsonRpcClient::rebind(Option<NetworkHandle>) -> RebindOutcome`.

**Liveness probe and network-lost hint.** The Dart worker runs all FFI calls through a single serialized
queue, so a `request("rpc.ping")` that is black-holed by a dead network would block a subsequent `rebind`
for the full request timeout (15 s). Two dedicated entry points avoid that:

- `QuicJsonRpcClient::ping(timeout)` / `quic_rpc_client_ping(client, timeout_ms)` — sends `rpc.ping` on the
  live connection with its **own** short timeout (the caller typically passes 3 s). It never triggers a
  reconnect and does not touch the configured request timeout; a failure just tells the caller the path
  is unhealthy. `timeout_ms == 0` falls back to `DEFAULT_REQUEST_TIMEOUT`. Returns `{"ok":true}` once the
  `pong` arrives, or `{"ok":false,"error":…}`.
- `QuicJsonRpcClient::mark_socket_suspect()` / `quic_rpc_client_mark_socket_suspect(client)` — hint from
  the platform (Android `onLost` for the network the socket is pinned to) that the current socket is
  dead. The next reconnect binds a fresh socket immediately, the same path as for `is_socket_path_error`.

Both symbols are optional from the Dart side: an older `.so` without them falls back to
`quic_rpc_client_request` for the probe and to a no-op for the hint.

**Which network carries the traffic right now.** `QuicJsonRpcClient::network_handle()` /
`quic_rpc_client_network_handle(client) -> u64` returns the handle the UDP socket is **actually** bound
to (`0` = default route). This differs from the handle the caller requested whenever the reconnect
fallback above dropped a pin to a dead network — the caller must treat its own request as an intention
and this value as the fact (the Flutter worker reads it on every pulse to label ping/pong truthfully).

## Run

```bash
QUIC_RPC_HEARTBEAT_SECS=10 QUIC_RPC_REQUEST_TIMEOUT_MS=5000 cargo run -p quic-jsonrpc-client -- 127.0.0.1:8443 ./quic-jsonrpc-client/certs/ca-root.pem '{"jsonrpc":"2.0","method":"login","id":1,"params":{"phone":"+380991112233","password":"secret","device":{"platform":"android","appVersion":"1.2.3","deviceId":"device-123","model":"Pixel 8","osVersion":"14"}}}' --timeout-ms 5000 --server-name mobilespace.app
```

## TLS trust

The second positional argument (and `ca_cert_path` in the FFI `quic_rpc_client_connect`) is a PEM file
with **trust anchors** — CA certificates, not the server's leaf certificate. Every certificate in the
file is added to the `RootCertStore`.

* `quic-jsonrpc-client/certs/ca-root.pem` — the trust bundle for the production `*.mobilespace.app`
  certificate chain served from `certs/server-fullchain.pem` (Go Daddy root + cross-root bundle).
  On Android this root bundle may already be in the system store, so the app may also rely on platform trust
  instead of bundling the file.
* For a self-signed dev certificate (`make gen-certs`, no `QUIC_RPC_CERT_PATH`) pass the self-signed
  `certs/server-fullchain.pem` itself — a self-signed cert is its own anchor.

`--server-name` / `QUIC_RPC_SERVER_NAME` must match a SAN of the server certificate
(`mobilespace.app` or a matching wildcard for production; default is `localhost`). Connecting by raw IP with the
production certificate fails with `NotValidForName`.

