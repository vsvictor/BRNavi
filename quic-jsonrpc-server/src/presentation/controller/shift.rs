use std::sync::Arc;

use serde_json::Value;

use crate::{
    application::ShiftService,
    domain::{RpcError, RpcResult},
    presentation::model::{AccessTokenParams, FinishShiftParams, StartShiftParams},
};

pub struct ShiftController {
    service: Arc<ShiftService>,
}

impl ShiftController {
    pub fn new(service: Arc<ShiftService>) -> Self {
        Self { service }
    }

    pub async fn handle(&self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "getShift" => self.service.get(&AccessTokenParams::parse(params)?).await,
            "startShift" => self.service.start(StartShiftParams::parse(params)?).await,
            "finishShift" => self.service.finish(FinishShiftParams::parse(params)?).await,
            _ => Err(RpcError::method_not_found()),
        }
    }
}
