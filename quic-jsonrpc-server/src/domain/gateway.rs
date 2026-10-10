//! Port: the upstream driver API (1C `driver-api`) the server proxies to.

use async_trait::async_trait;
use serde_json::Value;

use super::error::RpcResult;

#[async_trait]
pub trait DriverApiGateway: Send + Sync {
    /// `POST /auth/login/` with the raw login payload.
    async fn login(&self, payload: Value) -> RpcResult<Value>;

    /// `GET /driver/shift/`.
    async fn get_shift(&self, access_token: &str) -> RpcResult<Value>;

    /// `POST /driver/shift/start/`.
    async fn start_shift(&self, access_token: &str, body: Value) -> RpcResult<Value>;

    /// `POST /driver/shift/finish/`.
    async fn finish_shift(&self, access_token: &str, body: Value) -> RpcResult<Value>;
}
