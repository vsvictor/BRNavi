# Client trust anchors

| File | Contents | Purpose |
|---|---|---|
| `ca-root.pem` | `Go Daddy Root Certificate Authority - G2` + `GoDaddy TLS Root CA - R1` | Trust bundle for the production `*.mobilespace.app` certificate served by `quic-jsonrpc-server` from `../../certs/server-fullchain.pem` |

Pass this file as the `ca_cert_path` argument (CLI second positional argument, FFI `quic_rpc_client_connect`).
Only CA certificates belong here — never the server's leaf certificate (the leaf in `certs/server-fullchain.pem` is `CA:FALSE`
and is rejected as a trust anchor).

The chain the client validates: `*.mobilespace.app` → `GoDaddy TLS Intermediate CA DV - R1v1`
→ `GoDaddy TLS Root CA - R1` (cross-root) → `Go Daddy Root Certificate Authority - G2` (both roots are bundled in this file).

Android and macOS commonly trust GoDaddy roots in the system store, so the app can rely on platform trust instead of
bundling this file. Connect with server name `mobilespace.app`, not an IP.

