//! Application: the high-level RPC client — request retry, session capture/resume, heartbeat.

pub mod client;

pub use client::{
    CLIENT_CLOSE_CODE, CLOSE_DRAIN_TIMEOUT, ConnectOptions, DEFAULT_REQUEST_TIMEOUT, DEFAULT_REQUEST_TIMEOUT_MS,
    HEARTBEAT_FAILURES_BEFORE_RECONNECT, HeartbeatHandle, QuicJsonRpcClient, RebindOutcome,
};
