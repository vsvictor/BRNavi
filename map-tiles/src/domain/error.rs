//! Errors a tile lookup can produce. The host maps these onto its own protocol error codes.

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TileError {
    /// `z` above the configured maximum or `x`/`y` outside `0..2^z`.
    #[error("invalid tile coordinates: {0}")]
    InvalidCoord(String),
    /// Upstream answered 404.
    #[error("tile not found")]
    NotFound,
    /// Upstream answered with a non-success status other than 404.
    #[error("tile server returned HTTP {0}")]
    UpstreamStatus(u16),
    /// Upstream body exceeds the configured size limit.
    #[error("tile of {actual} bytes exceeds the limit of {max} bytes")]
    TooLarge { actual: usize, max: usize },
    /// Upstream returned something that is not an image.
    #[error("tile server returned non-image content-type {0}")]
    NonImage(String),
    /// Request could not be sent / no response (connect, timeout, ...). The message may contain
    /// the upstream URL — the host decides whether to forward it.
    #[error("tile request failed: {0}")]
    Transport(String),
    /// Response headers arrived but the body could not be read.
    #[error("tile response could not be read: {0}")]
    ResponseRead(String),
}

pub type TileResult<T> = std::result::Result<T, TileError>;

impl TileError {
    /// `true` for errors caused by the client's input rather than the upstream.
    pub fn is_client_error(&self) -> bool {
        matches!(self, TileError::InvalidCoord(_))
    }

    /// `true` when a retry of the same GET may succeed (transient upstream / transport failure).
    pub fn is_retryable(&self) -> bool {
        match self {
            TileError::Transport(_) | TileError::ResponseRead(_) => true,
            TileError::UpstreamStatus(status) => (500..=599).contains(status),
            _ => false,
        }
    }
}
