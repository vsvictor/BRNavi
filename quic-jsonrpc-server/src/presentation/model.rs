//! Request models: typed views over the raw JSON-RPC `params`, with validation.

use map_tiles::TileCoord;
use serde_json::{Map, Value};

use crate::{
    application::{
        auth::LoginCommand,
        shift::{FinishShiftCommand, StartShiftCommand},
    },
    domain::{Principal, RpcError, RpcResult, normalize_login, normalize_md5},
};

/// A parsed JSON-RPC 2.0 request envelope.
#[derive(Clone, Debug)]
pub struct RpcRequest {
    pub method: String,
    pub params: Option<Value>,
    /// `None` for notifications (no `id` member) — the server must not reply.
    pub id: Option<Value>,
}

impl RpcRequest {
    /// Validates the envelope. On failure returns the `id` to echo back (if any) with the error.
    pub fn parse(value: Value) -> std::result::Result<Self, (Option<Value>, RpcError)> {
        let Some(obj) = value.as_object() else {
            return Err((Some(Value::Null), RpcError::invalid_request()));
        };

        // JSON-RPC 2.0 §4: `id` MUST be a String, Number or Null. Anything else (object, array,
        // bool) is an invalid request; since we cannot echo such an id, reply with `null`.
        let id = obj.get("id").cloned();
        if let Some(id) = &id
            && !(id.is_string() || id.is_number() || id.is_null())
        {
            return Err((Some(Value::Null), RpcError::invalid_request()));
        }

        let version_ok = obj
            .get("jsonrpc")
            .and_then(Value::as_str)
            .map(|v| v == "2.0")
            .unwrap_or(false);
        if !version_ok {
            return Err((id, RpcError::invalid_request()));
        }

        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return Err((id, RpcError::invalid_request()));
        };

        Ok(Self {
            method: method.to_string(),
            params: obj.get("params").cloned(),
            id,
        })
    }

    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

fn object_params(params: Option<Value>) -> RpcResult<Map<String, Value>> {
    match params {
        Some(Value::Object(map)) => Ok(map),
        _ => Err(RpcError::invalid_params()),
    }
}

fn required_str<'a>(map: &'a Map<String, Value>, field: &str) -> RpcResult<&'a str> {
    map.get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(RpcError::invalid_params)
}

fn required_value(map: &Map<String, Value>, field: &str) -> RpcResult<Value> {
    map.get(field)
        .filter(|v| !v.is_null())
        .cloned()
        .ok_or_else(RpcError::invalid_params)
}

/// `login` / `rpc.login`: `{ login, password: <MD5 hex> }`, or the legacy phone/device payload.
///
/// Legacy phone payloads can still be parsed for explicitly injected compatibility adapters;
/// the production Mongo authenticator only accepts email + MD5.
pub struct LoginParams;

pub struct CreateAccountParams;

impl CreateAccountParams {
    pub fn parse(params: Option<Value>) -> RpcResult<crate::domain::authenticator::CreateAccountCommand> {
        let map = object_params(params)?;
        if map.len() != 3 {
            return Err(RpcError::invalid_params());
        }
        crate::domain::authenticator::CreateAccountCommand::new(
            required_str(&map, "name")?,
            required_str(&map, "email")?,
            required_str(&map, "password")?,
        )
    }
}

impl LoginParams {
    pub fn parse(params: Option<Value>) -> RpcResult<LoginCommand> {
        let mut map = object_params(params)?;
        let (phone, device_id) = if map.contains_key("login") {
            let login = normalize_login(required_str(&map, "login")?)?;
            let password = normalize_md5(required_str(&map, "password")?)?;
            map.insert("login".to_string(), Value::String(login.clone()));
            map.insert("password".to_string(), Value::String(password));
            // The two-field contract has no device identifier; sessions are account-scoped.
            (login, "email-login".to_string())
        } else {
            let phone = required_str(&map, "phone")?.to_string();
            let _password = required_str(&map, "password")?;
            let device = map
                .get("device")
                .and_then(Value::as_object)
                .ok_or_else(RpcError::invalid_params)?;
            (phone, required_str(device, "deviceId")?.to_string())
        };

        Ok(LoginCommand {
            principal: Principal::new(phone, device_id),
            upstream_payload: Value::Object(map),
        })
    }
}

/// `rpc.resume`, `rpc.session.info`, `rpc.session.debug`: `{ sessionId }`.
pub struct SessionIdParams;

impl SessionIdParams {
    pub fn parse(params: Option<Value>) -> RpcResult<String> {
        let map = object_params(params)?;
        Ok(required_str(&map, "sessionId")?.to_string())
    }

    /// `rpc.logout`: `{ sessionId? }` or no params at all — a client that never logged in may
    /// still want to close the connection cleanly.
    pub fn parse_optional(params: Option<Value>) -> RpcResult<Option<String>> {
        match params {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(map)) => Ok(map
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)),
            Some(_) => Err(RpcError::invalid_params()),
        }
    }
}

/// `getShift`: `{ accessToken }`.
pub struct AccessTokenParams;

impl AccessTokenParams {
    pub fn parse(params: Option<Value>) -> RpcResult<String> {
        let map = object_params(params)?;
        Ok(required_str(&map, "accessToken")?.to_string())
    }
}

/// `startShift`: `{ accessToken, deviceTime, sendTime, vehicleId, lat?, lng? }`.
pub struct StartShiftParams;

impl StartShiftParams {
    pub fn parse(params: Option<Value>) -> RpcResult<StartShiftCommand> {
        let map = object_params(params)?;
        Ok(StartShiftCommand {
            access_token: required_str(&map, "accessToken")?.to_string(),
            device_time: required_value(&map, "deviceTime")?,
            send_time: required_value(&map, "sendTime")?,
            vehicle_id: required_str(&map, "vehicleId")?.to_string(),
            lat: map.get("lat").cloned(),
            lng: map.get("lng").cloned(),
        })
    }
}

/// `finishShift`: `{ accessToken, deviceTime }`.
pub struct FinishShiftParams;

impl FinishShiftParams {
    pub fn parse(params: Option<Value>) -> RpcResult<FinishShiftCommand> {
        let map = object_params(params)?;
        Ok(FinishShiftCommand {
            access_token: required_str(&map, "accessToken")?.to_string(),
            device_time: required_value(&map, "deviceTime")?,
        })
    }
}

/// `map.getTile`: `{ z, x, y }` (non-negative integers; `z <= max_zoom`, `x, y < 2^z`).
pub struct TileParams;

impl TileParams {
    pub fn parse(params: Option<Value>, max_zoom: u8) -> RpcResult<TileCoord> {
        let map = object_params(params)?;
        let z = required_u64(&map, "z")?;
        let x = required_u64(&map, "x")?;
        let y = required_u64(&map, "y")?;
        let z = u8::try_from(z).map_err(|_| RpcError::invalid_params())?;
        let x = u32::try_from(x).map_err(|_| RpcError::invalid_params())?;
        let y = u32::try_from(y).map_err(|_| RpcError::invalid_params())?;
        TileCoord::new(z, x, y, max_zoom)
            .map_err(|err| RpcError::owned(crate::domain::codes::INVALID_PARAMS, format!("Invalid params: {err}")))
    }
}

fn required_u64(map: &Map<String, Value>, field: &str) -> RpcResult<u64> {
    map.get(field)
        .and_then(Value::as_u64)
        .ok_or_else(RpcError::invalid_params)
}

/// `math.add`: `[a, b]` or `{ a, b }`.
pub struct AddParams;

impl AddParams {
    pub fn parse(params: Option<Value>) -> RpcResult<(f64, f64)> {
        let err = RpcError::invalid_params;
        match params {
            Some(Value::Array(items)) if items.len() == 2 => {
                let a = items[0].as_f64().ok_or_else(err)?;
                let b = items[1].as_f64().ok_or_else(err)?;
                Ok((a, b))
            }
            Some(Value::Object(map)) => {
                let a = map.get("a").and_then(Value::as_f64).ok_or_else(err)?;
                let b = map.get("b").and_then(Value::as_f64).ok_or_else(err)?;
                Ok((a, b))
            }
            _ => Err(err()),
        }
    }
}
