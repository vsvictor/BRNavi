//! JSON-RPC 2.0 application error as seen by the business layer.

use std::borrow::Cow;

/// Well-known JSON-RPC / application error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;

    pub const SESSION_STORAGE: i64 = -32000;
    pub const SESSION_NOT_FOUND: i64 = -32001;
    pub const UPSTREAM_REQUEST_FAILED: i64 = -32002;
    pub const UPSTREAM_RESPONSE_READ_FAILED: i64 = -32003;
    /// Too many credential-bearing requests (`login` / `rpc.resume`) from one client.
    pub const RATE_LIMITED: i64 = -32004;
    pub const INVALID_CREDENTIALS: i64 = -32005;
    pub const AUTHENTICATION_UNAVAILABLE: i64 = -32006;
    pub const ACCOUNT_EXISTS: i64 = -32007;
    pub const ACCOUNT_CREATION_UNAVAILABLE: i64 = -32008;
    pub const ACCOUNT_CLEANUP_INCOMPLETE: i64 = -32009;

    pub const UPSTREAM_CLIENT_ERROR: i64 = -32010;
    pub const UPSTREAM_UNAUTHORIZED: i64 = -32011;
    pub const UPSTREAM_SERVER_ERROR: i64 = -32012;
    pub const UPSTREAM_FORBIDDEN: i64 = -32013;
    pub const UPSTREAM_OTHER_STATUS: i64 = -32014;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: Cow<'static, str>,
}

pub type RpcResult<T> = std::result::Result<T, RpcError>;

impl RpcError {
    pub const fn new(code: i64, message: &'static str) -> Self {
        Self {
            code,
            message: Cow::Borrowed(message),
        }
    }

    pub fn owned(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: Cow::Owned(message.into()),
        }
    }

    pub const fn invalid_params() -> Self {
        Self::new(codes::INVALID_PARAMS, "Invalid params")
    }

    pub const fn method_not_found() -> Self {
        Self::new(codes::METHOD_NOT_FOUND, "Method not found")
    }

    pub const fn invalid_request() -> Self {
        Self::new(codes::INVALID_REQUEST, "Invalid Request")
    }

    pub const fn parse_error() -> Self {
        Self::new(codes::PARSE_ERROR, "Parse error")
    }

    pub const fn session_not_found() -> Self {
        Self::new(codes::SESSION_NOT_FOUND, "Session not found")
    }

    pub const fn session_storage() -> Self {
        Self::new(codes::SESSION_STORAGE, "Internal session storage error")
    }

    pub const fn invalid_credentials() -> Self {
        Self::new(codes::INVALID_CREDENTIALS, "Invalid credentials")
    }

    pub const fn authentication_unavailable() -> Self {
        Self::new(codes::AUTHENTICATION_UNAVAILABLE, "Authentication unavailable")
    }

    pub const fn account_exists() -> Self {
        Self::new(codes::ACCOUNT_EXISTS, "Email already registered")
    }

    pub const fn account_creation_unavailable() -> Self {
        Self::new(codes::ACCOUNT_CREATION_UNAVAILABLE, "Account creation unavailable")
    }

    pub const fn account_cleanup_incomplete() -> Self {
        Self::new(
            codes::ACCOUNT_CLEANUP_INCOMPLETE,
            "Account creation failed; cleanup incomplete",
        )
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self::owned(
            codes::RATE_LIMITED,
            format!("Too many requests; retry after {retry_after_secs}s"),
        )
    }

    /// Maps an upstream HTTP status to the application error code space.
    pub fn code_for_upstream_status(status: u16) -> i64 {
        match status {
            401 => codes::UPSTREAM_UNAUTHORIZED,
            403 => codes::UPSTREAM_FORBIDDEN,
            500..=599 => codes::UPSTREAM_SERVER_ERROR,
            400..=499 => codes::UPSTREAM_CLIENT_ERROR,
            _ => codes::UPSTREAM_OTHER_STATUS,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}
