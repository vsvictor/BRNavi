//! Credential verification port, independent of the HTTP shift API.

use async_trait::async_trait;
use serde_json::Value;

use super::{DriverApiGateway, RpcError, RpcResult};

/// Validated registration input; never log or debug-format the password equivalent.
#[derive(Clone)]
pub struct CreateAccountCommand {
    pub name: String,
    pub email: String,
    pub password: String,
}

impl CreateAccountCommand {
    pub fn new(name: &str, email: &str, password: &str) -> RpcResult<Self> {
        let name = name.trim();
        if !(2..=100).contains(&name.chars().count())
            || name.chars().any(char::is_control)
            || name.split_whitespace().count() < 2
        {
            return Err(RpcError::invalid_params());
        }
        if password.len() != 32
            || !password
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(RpcError::invalid_params());
        }
        Ok(Self {
            name: name.to_string(),
            email: normalize_login(email)?,
            password: password.to_string(),
        })
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, payload: Value) -> RpcResult<Value>;

    async fn create_account(&self, _command: CreateAccountCommand) -> RpcResult<Value> {
        Err(RpcError::method_not_found())
    }
}

/// Compatibility adapter for explicitly injected legacy gateways (including test doubles).
/// The production composition root never uses this adapter.
pub struct GatewayAuthenticator(pub std::sync::Arc<dyn DriverApiGateway>);

#[async_trait]
impl Authenticator for GatewayAuthenticator {
    async fn authenticate(&self, payload: Value) -> RpcResult<Value> {
        self.0.login(payload).await
    }
}

pub fn normalize_login(login: &str) -> RpcResult<String> {
    let login = login.trim().to_ascii_lowercase();
    let Some((local, domain)) = login.split_once('@') else {
        return Err(super::RpcError::invalid_params());
    };
    if login.len() > 254
        || local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !local
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".!#$%&'*+/=?^_`{|}~-".contains(&b))
        || !domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b))
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || domain
            .split('.')
            .any(|label| label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-'))
    {
        return Err(super::RpcError::invalid_params());
    }
    Ok(login)
}

pub fn normalize_md5(password: &str) -> RpcResult<String> {
    if password.len() != 32 || !password.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(super::RpcError::invalid_params());
    }
    Ok(password.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_normalizes_full_name_and_email_but_requires_lowercase_md5() {
        let command = CreateAccountCommand::new(
            "  Тест Водій  ",
            " DRIVER@Example.com ",
            "5f4dcc3b5aa765d61d8327deb882cf99",
        )
        .unwrap();
        assert_eq!(command.name, "Тест Водій");
        assert_eq!(command.email, "driver@example.com");
        assert_eq!(command.password, "5f4dcc3b5aa765d61d8327deb882cf99");
        for (name, email, password) in [
            ("Single", "driver@example.com", "5f4dcc3b5aa765d61d8327deb882cf99"),
            ("First Last", "a@@b", "5f4dcc3b5aa765d61d8327deb882cf99"),
            ("First Last", "driver@example.com", "5F4DCC3B5AA765D61D8327DEB882CF99"),
        ] {
            assert!(CreateAccountCommand::new(name, email, password).is_err());
        }
    }

    #[test]
    fn credentials_are_normalized_and_validated() {
        assert_eq!(normalize_login(" Driver@Example.com ").unwrap(), "driver@example.com");
        for value in [
            "",
            "driver",
            "@example.com",
            "a@",
            "a@@b",
            "a b@c",
            "a@b/c",
            "a@.",
            "a@-b",
            "a..b@c",
        ] {
            assert!(normalize_login(value).is_err());
        }
        assert_eq!(
            normalize_md5("5F4DCC3B5AA765D61D8327DEB882CF99").unwrap(),
            "5f4dcc3b5aa765d61d8327deb882cf99"
        );
        for value in ["secret", "", "5f4dcc3b5aa765d61d8327deb882cf9z"] {
            assert!(normalize_md5(value).is_err());
        }
    }
}
