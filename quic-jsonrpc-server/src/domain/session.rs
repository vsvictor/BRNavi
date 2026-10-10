//! Session entity and the principal (phone + device) it belongs to.

/// Identifies *who* owns a session: one driver on one device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Principal {
    pub phone: String,
    pub device_id: String,
}

impl Principal {
    pub fn new(phone: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            phone: phone.into(),
            device_id: device_id.into(),
        }
    }

    /// Stable storage key — one active session per principal.
    pub fn key(&self) -> String {
        format!("{}|{}", self.phone, self.device_id)
    }
}

/// A resumable authenticated session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub principal: Principal,
    pub access_token: String,
}

impl Session {
    pub fn new(id: impl Into<String>, principal: Principal, access_token: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            principal,
            access_token: access_token.into(),
        }
    }
}
