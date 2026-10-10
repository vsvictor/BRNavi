//! Domain layer: pure business entities, value objects, errors and ports (traits).
//!
//! Nothing in this module depends on transport (QUIC), storage (MongoDB) or HTTP clients —
//! those are adapters living in [`crate::infrastructure`] and are plugged in through the traits
//! declared here.

pub mod authenticator;
pub mod error;
pub mod gateway;
pub mod repository;
pub mod session;
pub mod vehicle;

pub use authenticator::{Authenticator, GatewayAuthenticator, normalize_login, normalize_md5};
pub use error::{RpcError, RpcResult, codes};
pub use gateway::DriverApiGateway;
pub use repository::SessionRepository;
pub use session::{Principal, Session};
