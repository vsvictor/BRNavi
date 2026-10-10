//! C ABI exported for the Android/Flutter client. Symbol names are part of the public contract
//! (see `tool/check_quic_so.sh`) and must not change.
//!
//! The exported functions take raw pointers by design (C callers); each dereference is guarded by
//! a null check and documented with a SAFETY comment, so the blanket clippy lint is silenced here.
//!
//! Every entry point runs inside [`guarded`]: a Rust panic must never unwind across the `extern
//! "C"` boundary (that is undefined behaviour and in practice aborts the whole Flutter process).
//! Panics are caught, recorded via [`set_last_error`] and turned into the function's error value.
#![allow(clippy::not_unsafe_ptr_arg_deref)]
use crate::application::{ConnectOptions, HeartbeatHandle, QuicJsonRpcClient};
use anyhow::{Context, Result, bail};
use once_cell::sync::Lazy;
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString, c_char},
    net::{SocketAddr, ToSocketAddrs},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
pub struct NativeClient {
    runtime: tokio::runtime::Runtime,
    client: Arc<QuicJsonRpcClient>,
    heartbeat: Mutex<Option<HeartbeatHandle>>,
}

static LAST_ERROR: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

fn set_last_error(err: impl std::fmt::Display) {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = err.to_string();
    }
}

/// Extracts a human-readable message from a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic: <non-string payload>".to_string()
    }
}

/// Runs `f`, converting a panic into `on_panic(message)` instead of unwinding into C.
fn guarded<T>(f: impl FnOnce() -> T, on_panic: impl FnOnce(String) -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            let message = panic_message(&*payload);
            set_last_error(&message);
            on_panic(message)
        }
    }
}

fn json_status_ok(response: Option<Value>) -> *mut c_char {
    let payload = json!({ "ok": true, "response": response }).to_string();
    CString::new(payload)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

fn json_status_err(err: impl std::fmt::Display) -> *mut c_char {
    let payload = json!({ "ok": false, "error": err.to_string() }).to_string();
    CString::new(payload)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

fn cstr_arg<'a>(ptr: *const c_char, field: &str) -> Result<&'a str> {
    if ptr.is_null() {
        bail!("{field} is null");
    }

    let value = unsafe {
        // SAFETY: pointer is checked for null and expected to come from a C caller with NUL-terminated UTF-8.
        CStr::from_ptr(ptr)
    }
    .to_str()
    .with_context(|| format!("{field} is not valid UTF-8"))?;

    Ok(value)
}

/// Shared-reference view of a client handle. Callers must have checked for null.
///
/// All `QuicJsonRpcClient` methods take `&self`, and Dart may legitimately call e.g.
/// `is_connected` while a `request` is in flight on another thread, so the handle is only ever
/// borrowed shared — never `&mut`.
fn native<'a>(client: *mut NativeClient) -> &'a NativeClient {
    unsafe {
        // SAFETY: pointer is checked for null by the caller and expected to reference a valid
        // NativeClient (from Box::into_raw in quic_rpc_client_connect) until close is called.
        &*client
    }
}

fn resolve_server_addr(server_addr: &str) -> Result<SocketAddr> {
    server_addr
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve server address {server_addr}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("server address {server_addr} resolved to no addresses"))
}

fn connect_native(
    server_addr: *const c_char,
    server_name: *const c_char,
    ca_cert_path: *const c_char,
    network_handle: u64,
) -> *mut NativeClient {
    guarded(
        || {
            let result = (|| -> Result<NativeClient> {
                let server_addr = cstr_arg(server_addr, "server_addr")?;
                let server_name = cstr_arg(server_name, "server_name")?;
                let ca_cert_path = cstr_arg(ca_cert_path, "ca_cert_path")?;

                let socket = resolve_server_addr(server_addr)?;
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("failed to build Tokio runtime")?;

                let options = ConnectOptions {
                    network_handle: (network_handle != 0).then_some(network_handle),
                };
                let client = runtime.block_on(QuicJsonRpcClient::connect_with_options(
                    socket,
                    server_name,
                    Path::new(ca_cert_path),
                    options,
                ))?;

                Ok(NativeClient {
                    runtime,
                    client: Arc::new(client),
                    heartbeat: Mutex::new(None),
                })
            })();

            match result {
                Ok(client) => Box::into_raw(Box::new(client)),
                Err(err) => {
                    set_last_error(err);
                    std::ptr::null_mut()
                }
            }
        },
        |_| std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_connect(
    server_addr: *const c_char,
    server_name: *const c_char,
    ca_cert_path: *const c_char,
) -> *mut NativeClient {
    connect_native(server_addr, server_name, ca_cert_path, 0)
}

/// Like `quic_rpc_client_connect` but pins the UDP socket to an Android network
/// (`Network.getNetworkHandle()`), so a change of the *default* network — USB unplug, Wi‑Fi ↔
/// cellular, VPN — does not invalidate the socket. `network_handle == 0` behaves exactly like
/// `quic_rpc_client_connect`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_connect_on_network(
    server_addr: *const c_char,
    server_name: *const c_char,
    ca_cert_path: *const c_char,
    network_handle: u64,
) -> *mut NativeClient {
    connect_native(server_addr, server_name, ca_cert_path, network_handle)
}

/// Moves the client to a new UDP socket, optionally pinned to `network_handle` (0 = default
/// route). If the QUIC connection is still open it **migrates** (no handshake, session kept);
/// otherwise a reconnect (0‑RTT when possible) is performed on the new socket.
/// Returns `{"ok":true,"response":{"outcome":"migrated"|"reconnected"}}` or
/// `{"ok":false,"error":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_rebind(client: *mut NativeClient, network_handle: u64) -> *mut c_char {
    guarded(
        || {
            if client.is_null() {
                return json_status_err("client is null");
            }
            let native = native(client);
            let handle = (network_handle != 0).then_some(network_handle);
            match native.runtime.block_on(native.client.rebind(handle)) {
                Ok(outcome) => json_status_ok(Some(json!({ "outcome": outcome.as_str() }))),
                Err(err) => json_status_err(err),
            }
        },
        json_status_err,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_request(client: *mut NativeClient, request_json: *const c_char) -> *mut c_char {
    guarded(
        || {
            if client.is_null() {
                return json_status_err("client is null");
            }

            let request = match (|| -> Result<Value> {
                let request_json = cstr_arg(request_json, "request_json")?;
                serde_json::from_str::<Value>(request_json).context("invalid request JSON")
            })() {
                Ok(value) => value,
                Err(err) => return json_status_err(err),
            };

            let native = native(client);
            match native.runtime.block_on(native.client.request(request)) {
                Ok(response) => json_status_ok(response),
                Err(err) => json_status_err(err),
            }
        },
        json_status_err,
    )
}

/// Forces a reconnect on the existing client (same Tokio runtime, same session state).
/// Returns a JSON envelope `{"ok":true}` or `{"ok":false,"error":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_reconnect(client: *mut NativeClient) -> *mut c_char {
    guarded(
        || {
            if client.is_null() {
                return json_status_err("client is null");
            }
            let native = native(client);
            match native.runtime.block_on(native.client.reconnect()) {
                Ok(()) => json_status_ok(None),
                Err(err) => json_status_err(err),
            }
        },
        json_status_err,
    )
}

/// `true` while the underlying QUIC connection is open. Cheap: no network I/O.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_is_connected(client: *mut NativeClient) -> bool {
    guarded(
        || {
            if client.is_null() {
                return false;
            }
            let native = native(client);
            native.runtime.block_on(native.client.is_connected())
        },
        |_| false,
    )
}

/// Liveness probe (`rpc.ping`) bounded by `timeout_ms`, independent of the client-wide request
/// timeout. Never reconnects or retries. `timeout_ms == 0` falls back to the default request
/// timeout. Returns `{"ok":true}` on a pong or `{"ok":false,"error":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_ping(client: *mut NativeClient, timeout_ms: u64) -> *mut c_char {
    guarded(
        || {
            if client.is_null() {
                return json_status_err("client is null");
            }
            let timeout = if timeout_ms == 0 {
                crate::application::DEFAULT_REQUEST_TIMEOUT
            } else {
                Duration::from_millis(timeout_ms)
            };
            let native = native(client);
            match native.runtime.block_on(native.client.ping(timeout)) {
                Ok(()) => json_status_ok(None),
                Err(err) => json_status_err(err),
            }
        },
        json_status_err,
    )
}

/// Marks the current UDP socket as dead so the next reconnect binds a fresh one at once
/// (call when the OS reports the pinned network was lost). Cheap, no I/O. Returns `false` only
/// for a null client.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_mark_socket_suspect(client: *mut NativeClient) -> bool {
    guarded(
        || {
            if client.is_null() {
                return false;
            }
            native(client).client.mark_socket_suspect();
            true
        },
        |_| false,
    )
}

/// Android network handle the UDP socket is **actually** bound to right now. `0` = default route
/// (never pinned, or the pin was dropped by the reconnect fallback after the pinned network
/// died). This is the ground truth for "which channel carries the traffic"; the handle the
/// caller *requested* in `connect_on_network`/`rebind` may be stale. Cheap, no I/O.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_network_handle(client: *mut NativeClient) -> u64 {
    guarded(
        || {
            if client.is_null() {
                return 0;
            }
            let native = native(client);
            native.runtime.block_on(native.client.network_handle()).unwrap_or(0)
        },
        |_| 0,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_set_request_timeout(client: *mut NativeClient, timeout_ms: u64) -> bool {
    guarded(
        || {
            if client.is_null() {
                return false;
            }
            let native = native(client);
            let timeout = if timeout_ms == 0 {
                None
            } else {
                Some(Duration::from_millis(timeout_ms))
            };
            native.runtime.block_on(native.client.set_request_timeout(timeout));
            true
        },
        |_| false,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_set_heartbeat(client: *mut NativeClient, interval_secs: u64) -> bool {
    guarded(
        || {
            if client.is_null() || interval_secs == 0 {
                return false;
            }
            let native = native(client);
            match native.heartbeat.lock() {
                Ok(mut guard) => {
                    // `spawn_heartbeat` uses `tokio::spawn`, so it must be called
                    // from within a Tokio runtime context.
                    let heartbeat = native.runtime.block_on(async {
                        native
                            .client
                            .clone()
                            .spawn_heartbeat(Duration::from_secs(interval_secs))
                    });
                    *guard = Some(heartbeat);
                    true
                }
                Err(_) => false,
            }
        },
        |_| false,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_clear_heartbeat(client: *mut NativeClient) {
    guarded(
        || {
            if client.is_null() {
                return;
            }
            if let Ok(mut guard) = native(client).heartbeat.lock() {
                *guard = None;
            }
        },
        |_| (),
    )
}

/// Explicit logout: sends `rpc.logout` (with the captured session id, if any) so the server
/// forgets the session, then closes the QUIC connection. The client handle stays valid — the
/// next request transparently reconnects. Returns `{"ok":true,"response":...}` or
/// `{"ok":false,"error":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_logout(client: *mut NativeClient) -> *mut c_char {
    guarded(
        || {
            if client.is_null() {
                return json_status_err("client is null");
            }
            let native = native(client);

            // A running heartbeat would immediately reconnect after logout — stop it first.
            if let Ok(mut guard) = native.heartbeat.lock() {
                *guard = None;
            }

            match native.runtime.block_on(native.client.logout()) {
                Ok(response) => json_status_ok(response),
                Err(err) => json_status_err(err),
            }
        },
        json_status_err,
    )
}

/// Releases the client. The QUIC connection is closed gracefully (CONNECTION_CLOSE is flushed)
/// so the server does not have to wait for its idle timeout; the server-side session is left
/// intact and can be resumed by a new client. Use `quic_rpc_client_logout` to end the session.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_close(client: *mut NativeClient) {
    guarded(
        || {
            if client.is_null() {
                return;
            }

            let native = unsafe {
                // SAFETY: pointer must come from Box::into_raw in quic_rpc_client_connect and must be consumed once here.
                Box::from_raw(client)
            };

            if let Ok(mut guard) = native.heartbeat.lock() {
                *guard = None;
            }
            native.runtime.block_on(native.client.close());
            drop(native);
        },
        |_| (),
    )
}

/// Fetches one map tile through the server (`map.getTile`) and returns the **decoded image
/// bytes** (PNG by default) so Dart can wrap them in a `Uint8List` without a base64 round trip.
///
/// * `z`, `x`, `y` — slippy-map coordinates (`z <= 255`, `x, y < 2^z`).
/// * `out_len` — receives the buffer length (required).
/// * `out_content_type` — optional; receives a NUL-terminated MIME string that must be released
///   with `quic_rpc_string_free`.
///
/// Returns `NULL` on failure; the reason is available via `quic_rpc_last_error_message`. The
/// returned buffer must be released with `quic_rpc_bytes_free(ptr, len)`.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_client_get_tile(
    client: *mut NativeClient,
    z: u32,
    x: u32,
    y: u32,
    out_len: *mut usize,
    out_content_type: *mut *mut c_char,
) -> *mut u8 {
    guarded(
        || {
            if client.is_null() {
                set_last_error("client is null");
                return std::ptr::null_mut();
            }
            if out_len.is_null() {
                set_last_error("out_len is null");
                return std::ptr::null_mut();
            }
            let Ok(z) = u8::try_from(z) else {
                set_last_error("zoom out of range");
                return std::ptr::null_mut();
            };

            let native = native(client);
            match native.runtime.block_on(native.client.get_tile(z, x, y)) {
                Ok(tile) => {
                    if !out_content_type.is_null() {
                        let ct = CString::new(tile.content_type)
                            .map(CString::into_raw)
                            .unwrap_or(std::ptr::null_mut());
                        unsafe {
                            // SAFETY: non-null out pointer supplied by the C caller.
                            *out_content_type = ct;
                        }
                    }
                    let boxed: Box<[u8]> = tile.bytes.into_boxed_slice();
                    let len = boxed.len();
                    unsafe {
                        // SAFETY: non-null out pointer supplied by the C caller.
                        *out_len = len;
                    }
                    Box::into_raw(boxed) as *mut u8
                }
                Err(err) => {
                    set_last_error(err);
                    std::ptr::null_mut()
                }
            }
        },
        |_| std::ptr::null_mut(),
    )
}

/// Releases a buffer returned by `quic_rpc_client_get_tile`. `len` must be the value written
/// to `out_len` for that buffer.
#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_bytes_free(ptr: *mut u8, len: usize) {
    guarded(
        || {
            if ptr.is_null() {
                return;
            }
            unsafe {
                // SAFETY: pointer/len pair must come from Box::<[u8]>::into_raw in
                // quic_rpc_client_get_tile and be freed exactly once.
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
            }
        },
        |_| (),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_last_error_message() -> *mut c_char {
    guarded(
        || {
            let message = LAST_ERROR
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_else(|_| "unknown error".to_string());

            CString::new(message)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut())
        },
        |_| std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn quic_rpc_string_free(value: *mut c_char) {
    guarded(
        || {
            if value.is_null() {
                return;
            }
            unsafe {
                // SAFETY: pointer must come from CString::into_raw in this library.
                drop(CString::from_raw(value));
            }
        },
        |_| (),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ip_and_hostname_server_addresses() {
        assert_eq!(
            resolve_server_addr("127.0.0.1:8443").unwrap(),
            "127.0.0.1:8443".parse().unwrap()
        );

        let localhost = resolve_server_addr("localhost:8443").unwrap();
        assert!(localhost.ip().is_loopback());
        assert_eq!(localhost.port(), 8443);
    }

    #[test]
    fn guarded_turns_panics_into_fallback_and_records_last_error() {
        let value = guarded(|| -> i32 { panic!("boom") }, |_| -1);
        assert_eq!(value, -1);
        assert_eq!(LAST_ERROR.lock().unwrap().as_str(), "panic: boom");

        let value = guarded(|| 42, |_| -1);
        assert_eq!(value, 42);
    }

    #[test]
    fn null_client_is_rejected_without_panicking() {
        let ptr = quic_rpc_client_request(std::ptr::null_mut(), std::ptr::null());
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        quic_rpc_string_free(ptr);
        assert!(text.contains("client is null"));
        assert!(!quic_rpc_client_is_connected(std::ptr::null_mut()));
        quic_rpc_client_close(std::ptr::null_mut());
    }

    #[test]
    fn rebind_and_connect_on_network_reject_null_without_panicking() {
        let ptr = quic_rpc_client_rebind(std::ptr::null_mut(), 42);
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        quic_rpc_string_free(ptr);
        assert!(text.contains("client is null"));

        let client = quic_rpc_client_connect_on_network(std::ptr::null(), std::ptr::null(), std::ptr::null(), 7);
        assert!(client.is_null());
        assert!(LAST_ERROR.lock().unwrap().contains("server_addr is null"));
    }

    #[test]
    fn get_tile_rejects_null_client_and_bytes_free_tolerates_null() {
        let mut len = 7usize;
        let ptr = quic_rpc_client_get_tile(std::ptr::null_mut(), 1, 0, 0, &mut len, std::ptr::null_mut());
        assert!(ptr.is_null());
        assert_eq!(LAST_ERROR.lock().unwrap().as_str(), "client is null");
        quic_rpc_bytes_free(std::ptr::null_mut(), 0);

        // Round-trip a buffer through the same allocation scheme the FFI uses.
        let boxed: Box<[u8]> = vec![1u8, 2, 3].into_boxed_slice();
        let n = boxed.len();
        let raw = Box::into_raw(boxed) as *mut u8;
        quic_rpc_bytes_free(raw, n);
    }
}
