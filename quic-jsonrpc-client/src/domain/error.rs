use std::time::Duration;

/// A request exceeded the caller-configured deadline. Distinguished from transport errors
/// because a slow server must not trigger a blind reconnect.
#[derive(Debug)]
pub struct RequestTimeoutError(pub Duration);

impl std::fmt::Display for RequestTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request timed out after {}s", self.0.as_secs_f64())
    }
}

impl std::error::Error for RequestTimeoutError {}
