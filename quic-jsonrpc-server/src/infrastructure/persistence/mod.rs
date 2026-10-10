//! Session repository adapters.

pub mod memory;
pub mod mongo;
pub mod users;
pub mod vehicles;

use base64::Engine;
use rand::TryRngCore;

pub use memory::InMemorySessionRepository;
pub use mongo::MongoSessionRepository;
pub use users::MongoAuthenticator;

/// Entropy of a session id in bytes (192 bits).
const SESSION_ID_BYTES: usize = 24;

/// Generates an unguessable session id (`sess-<base64url, 32 chars>`).
///
/// The id is a bearer credential: `rpc.resume {sessionId}` hands out the access token, so it
/// must come from a CSPRNG — *not* from `ObjectId` (timestamp + per-process constant + counter),
/// which lets an attacker enumerate neighbouring sessions.
///
/// Panics only if the OS entropy source is unavailable, which is unrecoverable for a server that
/// mints credentials.
pub fn generate_session_id() -> String {
    let mut bytes = [0u8; SESSION_ID_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random number generator unavailable");
    format!(
        "sess-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_unique_and_well_formed() {
        let a = generate_session_id();
        let b = generate_session_id();
        assert_ne!(a, b);
        assert!(a.starts_with("sess-"));
        assert_eq!(a.len(), "sess-".len() + 32);
        assert!(
            a["sess-".len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }
}
