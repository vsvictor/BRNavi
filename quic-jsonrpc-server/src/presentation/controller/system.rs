//! Built-in diagnostics: `rpc.ping`, `rpc.health`, `echo`, `math.add`.

use std::sync::Arc;

use serde_json::{Value, json};
use tracing::debug;

use crate::{
    application::Metrics,
    domain::{RpcError, RpcResult},
    presentation::model::AddParams,
};

pub struct SystemController {
    metrics: Arc<Metrics>,
}

impl SystemController {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }

    pub async fn handle(&self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "rpc.ping" => Ok(json!("pong")),
            "rpc.health" => Ok(self.metrics.snapshot()),
            "echo" => Ok(params.unwrap_or(Value::Null)),
            "math.add" => {
                let (a, b) = AddParams::parse(params)?;
                debug!("math.add evaluated: a={a} b={b} sum={}", a + b);
                Ok(json!(a + b))
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}
