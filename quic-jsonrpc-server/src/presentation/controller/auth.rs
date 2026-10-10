use std::sync::Arc;

use serde_json::Value;

use crate::{
    application::AuthService,
    domain::{RpcError, RpcResult},
    presentation::model::{CreateAccountParams, LoginParams, SessionIdParams},
};

pub struct AuthController {
    service: Arc<AuthService>,
    session_debug_enabled: bool,
}

impl AuthController {
    pub fn new(service: Arc<AuthService>, session_debug_enabled: bool) -> Self {
        Self {
            service,
            session_debug_enabled,
        }
    }

    pub async fn handle(&self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "createAccount" => self.service.create_account(CreateAccountParams::parse(params)?).await,
            "login" | "rpc.login" => self.service.login(LoginParams::parse(params)?).await,
            "rpc.resume" => self.service.resume(&SessionIdParams::parse(params)?).await,
            "logout" | "rpc.logout" => {
                self.service
                    .logout(SessionIdParams::parse_optional(params)?.as_deref())
                    .await
            }
            "rpc.session.info" => self.service.info(&SessionIdParams::parse(params)?).await,
            "rpc.session.debug" if self.session_debug_enabled => {
                self.service.debug(&SessionIdParams::parse(params)?).await
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}
