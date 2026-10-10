//! JSON-RPC 2.0 over QUIC client with transparent reconnect and session resume.
//!
//! Layout (Clean Architecture):
//!
//! ```text
//! presentation (C FFI)  ──▶  application (QuicJsonRpcClient)  ──▶  domain (protocol, errors)
//!                                        ▲
//!                       infrastructure (QUIC transport, TLS) ┘
//! ```
//!
//! The public Rust API (`QuicJsonRpcClient`, `HeartbeatHandle`, timeout constants) and the
//! exported C symbols are re-exported at the crate root and unchanged.

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod presentation;

pub use application::{
    ConnectOptions, DEFAULT_REQUEST_TIMEOUT, DEFAULT_REQUEST_TIMEOUT_MS, HeartbeatHandle, QuicJsonRpcClient,
    RebindOutcome,
};
pub use infrastructure::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_MAX_IDLE_TIMEOUT, DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT,
    NetworkHandle,
};
pub use presentation::NativeClient;
pub use presentation::ffi::*;
