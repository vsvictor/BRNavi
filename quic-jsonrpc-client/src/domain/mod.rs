//! Domain: JSON-RPC protocol value objects and client-side error types. No I/O.

pub mod error;
pub mod protocol;

pub use error::RequestTimeoutError;
pub use protocol::{
    JsonRpcRequest, SESSION_NOT_FOUND_CODE, TileImage, extract_session_id, extract_tile, is_ping_pong_response,
    is_session_not_found,
};
