//! JSON-RPC 2.0 helpers used by the client.

use serde_json::{Value, json};

/// Server error code meaning the session id is unknown / expired.
pub const SESSION_NOT_FOUND_CODE: i64 = -32001;

/// Builders for the requests the client emits on its own behalf.
pub struct JsonRpcRequest;

impl JsonRpcRequest {
    pub fn ping(id: &str) -> Value {
        json!({ "jsonrpc": "2.0", "method": "rpc.ping", "id": id })
    }

    pub fn resume(session_id: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "rpc.resume",
            "id": "resume-after-reconnect",
            "params": { "sessionId": session_id }
        })
    }

    /// `rpc.logout`: asks the server to forget the session and close the connection.
    pub fn logout(session_id: Option<&str>) -> Value {
        match session_id {
            Some(id) => json!({
                "jsonrpc": "2.0",
                "method": "rpc.logout",
                "id": "logout",
                "params": { "sessionId": id }
            }),
            None => json!({ "jsonrpc": "2.0", "method": "rpc.logout", "id": "logout" }),
        }
    }

    /// `map.getTile`: fetches one slippy-map tile (`z/x/y`) through the server-side OSM proxy.
    pub fn get_tile(z: u8, x: u32, y: u32) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "map.getTile",
            "id": format!("tile-{z}-{x}-{y}"),
            "params": { "z": z, "x": x, "y": y }
        })
    }
}

/// `map.getTile` result as decoded by the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileImage {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub content_type: String,
    pub etag: Option<String>,
    pub from_cache: bool,
    /// Decoded image bytes (PNG by default).
    pub bytes: Vec<u8>,
}

/// Pulls a [`TileImage`] out of a `map.getTile` response. Returns the JSON-RPC error object
/// (as `Err`) when the server replied with one.
pub fn extract_tile(response: &Value) -> std::result::Result<TileImage, Value> {
    if let Some(error) = response.get("error") {
        return Err(error.clone());
    }
    let result = response
        .get("result")
        .ok_or_else(|| json!({"message": "missing result"}))?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| json!({"message": "missing data"}))?;
    let bytes = base64_decode(data).map_err(|e| json!({"message": format!("invalid base64: {e}")}))?;
    Ok(TileImage {
        z: result.get("z").and_then(Value::as_u64).unwrap_or(0) as u8,
        x: result.get("x").and_then(Value::as_u64).unwrap_or(0) as u32,
        y: result.get("y").and_then(Value::as_u64).unwrap_or(0) as u32,
        content_type: result
            .get("contentType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string(),
        etag: result.get("etag").and_then(Value::as_str).map(str::to_string),
        from_cache: result.get("fromCache").and_then(Value::as_bool).unwrap_or(false),
        bytes,
    })
}

/// Minimal standard-alphabet base64 decoder (with or without `=` padding). Kept dependency-free
/// so the FFI `.so` does not grow.
fn base64_decode(input: &str) -> std::result::Result<Vec<u8>, &'static str> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            acc |= val(*b).ok_or("invalid character")? << (18 - 6 * i);
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]),
            3 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8]),
            2 => out.push((acc >> 16) as u8),
            _ => return Err("truncated input"),
        }
    }
    Ok(out)
}

/// Pulls `result.session.sessionId` out of a login / resume response.
pub fn extract_session_id(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|result| result.get("session"))
        .and_then(|session| session.get("sessionId"))
        .and_then(Value::as_str)
}

pub fn is_session_not_found(response: &Value) -> bool {
    response
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        == Some(SESSION_NOT_FOUND_CODE)
}

/// Strict validation for heartbeat ping responses.
///
/// A valid ping response must be a JSON-RPC success envelope with
/// `result == "pong"` and without an `error` field.
pub fn is_ping_pong_response(response: Option<&Value>) -> bool {
    let Some(payload) = response else {
        return false;
    };
    if payload.get("error").is_some() {
        return false;
    }
    payload.get("result").and_then(Value::as_str) == Some("pong")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_with_and_without_padding() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zg").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(base64_decode("iVBORw0KGgo=").unwrap(), b"\x89PNG\r\n\x1a\n");
        assert!(base64_decode("Z*==").is_err());
        assert!(base64_decode("Z").is_err());
    }

    #[test]
    fn extracts_tile_from_response() {
        let response = json!({
            "jsonrpc": "2.0", "id": "tile-1-0-0",
            "result": {"z": 1, "x": 0, "y": 0, "contentType": "image/png", "etag": "\"e\"",
                       "fromCache": true, "size": 3, "data": "Zm9v"}
        });
        let tile = extract_tile(&response).unwrap();
        assert_eq!((tile.z, tile.x, tile.y), (1, 0, 0));
        assert_eq!(tile.bytes, b"foo");
        assert!(tile.from_cache);
        assert_eq!(tile.etag.as_deref(), Some("\"e\""));

        let err =
            extract_tile(&json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32010, "message": "nf"}})).unwrap_err();
        assert_eq!(err["code"], json!(-32010));
    }

    #[test]
    fn validates_ping_pong_response_strictly() {
        assert!(is_ping_pong_response(Some(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": "pong"
        }))));

        assert!(!is_ping_pong_response(None));
        assert!(!is_ping_pong_response(Some(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": "ok"
        }))));
        assert!(!is_ping_pong_response(Some(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32000, "message": "oops"}
        }))));
    }
}
