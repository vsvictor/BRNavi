//! Infrastructure layer: adapters implementing the domain ports plus everything that touches the
//! outside world (environment, MongoDB, HTTP upstream, TLS, QUIC transport).

pub mod config;
pub mod env;
pub mod http;
pub mod logging;
pub mod persistence;
pub mod quic;
pub mod tls;

pub use config::{ServerOptions, SessionBackend, TlsPaths, UpstreamOptions};
