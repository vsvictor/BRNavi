//! Infrastructure: QUIC transport, UDP socket binding and TLS configuration.

mod logging;
pub mod quic;
pub mod socket;
pub mod tls;

pub use quic::{QuicTransport, Reconnected, send_request_on_connection};
pub use socket::{NetworkHandle, bind_udp_socket, is_socket_path_error};
pub use tls::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_MAX_IDLE_TIMEOUT, DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT,
    build_client_config,
};
