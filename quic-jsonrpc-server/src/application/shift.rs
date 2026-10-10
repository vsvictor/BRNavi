//! Driver shift use cases — thin pass-through to the upstream gateway with validated input.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::domain::{DriverApiGateway, RpcResult};

#[derive(Clone, Debug)]
pub struct StartShiftCommand {
    pub access_token: String,
    pub device_time: Value,
    pub send_time: Value,
    pub vehicle_id: String,
    pub lat: Option<Value>,
    pub lng: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct FinishShiftCommand {
    pub access_token: String,
    pub device_time: Value,
}

pub struct ShiftService {
    gateway: Arc<dyn DriverApiGateway>,
}

impl ShiftService {
    pub fn new(gateway: Arc<dyn DriverApiGateway>) -> Self {
        Self { gateway }
    }

    pub async fn get(&self, access_token: &str) -> RpcResult<Value> {
        self.gateway.get_shift(access_token).await
    }

    pub async fn start(&self, command: StartShiftCommand) -> RpcResult<Value> {
        let mut body = Map::new();
        body.insert("deviceTime".into(), command.device_time);
        body.insert("sendTime".into(), command.send_time);
        body.insert("vehicleId".into(), Value::String(command.vehicle_id));
        if let Some(lat) = command.lat {
            body.insert("lat".into(), lat);
        }
        if let Some(lng) = command.lng {
            body.insert("lng".into(), lng);
        }
        self.gateway
            .start_shift(&command.access_token, Value::Object(body))
            .await
    }

    pub async fn finish(&self, command: FinishShiftCommand) -> RpcResult<Value> {
        let body = json!({ "deviceTime": command.device_time });
        self.gateway.finish_shift(&command.access_token, body).await
    }
}
