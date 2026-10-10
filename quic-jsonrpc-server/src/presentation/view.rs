//! View: JSON-RPC 2.0 response envelopes.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::domain::{RpcError, RpcResult};

pub struct JsonRpcView;

impl JsonRpcView {
    pub fn success(id: Value, result: Value) -> Value {
        let mut response = BTreeMap::new();
        response.insert("jsonrpc", json!("2.0"));
        response.insert("result", result);
        response.insert("id", id);
        json!(response)
    }

    pub fn error(id: Value, error: &RpcError) -> Value {
        json!({
            "jsonrpc": "2.0",
            "error": {
                "code": error.code,
                "message": error.message.as_ref(),
            },
            "id": id,
        })
    }

    pub fn render(id: Value, result: RpcResult<Value>) -> Value {
        match result {
            Ok(value) => Self::success(id, value),
            Err(err) => Self::error(id, &err),
        }
    }

    /// Wire format: one JSON document per line.
    pub fn to_line(value: &Value) -> String {
        value.to_string()
    }
}
