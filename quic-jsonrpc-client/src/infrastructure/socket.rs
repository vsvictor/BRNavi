//! UDP socket creation for the QUIC endpoint.
//!
//! On Android every socket is, by default, routed through the *default* network. When the
//! default network flips (USB unplug → `rndis0`/`usb0` disappears, Wi‑Fi ↔ cellular, VPN up/down)
//! the kernel/netd invalidates existing UDP bindings: sends start failing with
//! `ENETUNREACH`/`ECONNRESET` and inbound packets are dropped. Binding the socket explicitly to a
//! `Network` handle (`android_setsocknetwork`, API 23+) pins it to one interface so a change of
//! the default network does not affect it, and lets the app pick Wi‑Fi or cellular deliberately.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

use anyhow::{Context, Result};

/// Opaque Android `Network` handle (`Network.getNetworkHandle()`), `0` = unbound.
pub type NetworkHandle = u64;

/// Binds a fresh UDP socket for the QUIC endpoint. `network_handle` (Android only) pins the
/// socket to that network; `None`/`0` leaves it on the default route.
pub fn bind_udp_socket(remote: SocketAddr, network_handle: Option<NetworkHandle>) -> Result<UdpSocket> {
    let bind_addr: SocketAddr = if remote.is_ipv6() {
        "[::]:0".parse().expect("valid v6 bind")
    } else {
        "0.0.0.0:0".parse().expect("valid v4 bind")
    };
    let socket = UdpSocket::bind(bind_addr).context("failed to bind QUIC client socket")?;

    if let Some(handle) = network_handle.filter(|h| *h != 0) {
        bind_to_network(&socket, handle)
            .with_context(|| format!("failed to bind QUIC client socket to network handle {handle}"))?;
    }

    Ok(socket)
}

#[cfg(target_os = "android")]
fn bind_to_network(socket: &UdpSocket, handle: NetworkHandle) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // `android_setsocknetwork` lives in libandroid.so (API 23+). It is resolved dynamically so the
    // library still loads on older devices / in host unit tests and does not need `-landroid`.
    type SetSockNetwork = unsafe extern "C" fn(u64, libc::c_int) -> libc::c_int;

    static RESOLVED: once_cell::sync::Lazy<Option<SetSockNetwork>> = once_cell::sync::Lazy::new(|| unsafe {
        // SAFETY: standard dlopen/dlsym usage with NUL-terminated literals; the symbol has the
        // documented signature `int android_setsocknetwork(net_handle_t, int)`.
        let lib = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            return None;
        }
        let sym = libc::dlsym(lib, c"android_setsocknetwork".as_ptr());
        if sym.is_null() {
            return None;
        }
        Some(std::mem::transmute::<*mut libc::c_void, SetSockNetwork>(sym))
    });

    let Some(set_sock_network) = *RESOLVED else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "android_setsocknetwork is not available on this device",
        ));
    };

    // SAFETY: fd is a valid open socket owned by `socket`; the function only touches that fd.
    let rc = unsafe { set_sock_network(handle, socket.as_raw_fd()) };
    if rc == 0 {
        Ok(())
    } else {
        // Bionic returns `-errno` here (not -1 + errno).
        Err(io::Error::from_raw_os_error(rc.abs()))
    }
}

#[cfg(not(target_os = "android"))]
fn bind_to_network(_socket: &UdpSocket, _handle: NetworkHandle) -> io::Result<()> {
    // Not an error on other platforms: the handle is advisory and simply ignored.
    Ok(())
}

/// `true` when the error chain contains an OS-level symptom of a dead UDP path (the interface
/// vanished or the kernel reset the socket binding). Such errors can never be fixed by waiting —
/// the socket must be rebound and the session re-established immediately.
pub fn is_socket_path_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io_err) = cause.downcast_ref::<io::Error>()
            && is_socket_path_io_error(io_err)
        {
            return true;
        }
        if let Some(conn_err) = cause.downcast_ref::<quinn::ConnectionError>()
            && is_socket_path_connection_error(conn_err)
        {
            return true;
        }
    }
    // quinn/anyhow sometimes flatten the io::Error into text only.
    let text = err.to_string().to_ascii_lowercase();
    text.contains("network is unreachable")
        || text.contains("network is down")
        || text.contains("connection reset")
        || text.contains("no route to host")
        || text.contains("host is unreachable")
}

fn is_socket_path_io_error(err: &io::Error) -> bool {
    match err.kind() {
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::NetworkUnreachable
        | io::ErrorKind::NetworkDown
        | io::ErrorKind::HostUnreachable
        | io::ErrorKind::AddrNotAvailable => true,
        _ => matches!(
            err.raw_os_error(),
            Some(libc::ENETUNREACH)
                | Some(libc::ENETDOWN)
                | Some(libc::EHOSTUNREACH)
                | Some(libc::ECONNRESET)
                | Some(libc::EADDRNOTAVAIL)
        ),
    }
}

fn is_socket_path_connection_error(err: &quinn::ConnectionError) -> bool {
    // A locally detected dead path surfaces as a transport/timeout close; the peer never answered.
    matches!(err, quinn::ConnectionError::TimedOut | quinn::ConnectionError::Reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_without_network_handle() {
        let remote: SocketAddr = "127.0.0.1:4433".parse().unwrap();
        let socket = bind_udp_socket(remote, None).expect("bind");
        assert!(socket.local_addr().unwrap().ip().is_ipv4());
        let socket = bind_udp_socket(remote, Some(0)).expect("bind with zero handle");
        assert!(socket.local_addr().is_ok());
    }

    #[test]
    fn classifies_socket_path_errors() {
        let err = anyhow::Error::from(io::Error::from_raw_os_error(libc::ENETUNREACH)).context("send failed");
        assert!(is_socket_path_error(&err));
        let err = anyhow::Error::from(io::Error::from_raw_os_error(libc::ECONNRESET));
        assert!(is_socket_path_error(&err));
        let err = anyhow::anyhow!("QUIC reconnect handshake failed: Network is unreachable (os error 101)");
        assert!(is_socket_path_error(&err));
        let err = anyhow::anyhow!("server returned non-JSON response");
        assert!(!is_socket_path_error(&err));
    }
}
