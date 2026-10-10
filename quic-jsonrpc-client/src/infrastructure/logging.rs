use serde_json::Value;

pub(super) fn log_response(request: &Value, response: Option<&Value>, status: &str) {
    let mut entry = serde_json::json!({
        "method": request.get("method"),
        "id": request.get("id"),
        "status": status,
        "response": response,
    });
    redact(&mut entry);
    write_log(&format!("QUIC RPC server response: {entry}"));
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let normalized = key.replace(['_', '-'], "").to_ascii_lowercase();
                if ["password", "token", "secret", "sessionid", "authorization", "apikey"]
                    .iter()
                    .any(|sensitive| normalized.contains(sensitive))
                    || matches!(normalized.as_str(), "pin" | "otp")
                {
                    *child = Value::String("<redacted>".into());
                } else if matches!(normalized.as_str(), "login" | "email" | "phone" | "msisdn" | "name") {
                    *child = Value::String("<masked>".into());
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

#[cfg(target_os = "android")]
fn write_log(message: &str) {
    use std::ffi::{CString, c_char, c_int};

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    // Logcat has a per-entry byte limit; keep every byte on UTF-8 boundaries.
    for (index, chunk) in log_chunks(message, 3_000).iter().enumerate() {
        let text = CString::new(format!("[part {}] {chunk}", index + 1))
            .expect("serialized JSON log cannot contain NUL");
        unsafe {
            __android_log_write(4, c"XRNaviBackend".as_ptr(), text.as_ptr());
        }
    }
}

#[cfg(not(target_os = "android"))]
fn write_log(message: &str) {
    eprintln!("{message}");
}

#[cfg(any(target_os = "android", test))]
fn log_chunks(mut message: &str, limit: usize) -> Vec<&str> {
    assert!(limit >= 4);
    let mut chunks = Vec::new();
    while !message.is_empty() {
        let mut end = message.len().min(limit);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = message.split_at(end);
        chunks.push(chunk);
        message = rest;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_credentials_without_removing_response_data() {
        let mut response = serde_json::json!({
            "result": {
                "status": "ok",
                "login": "driver@example.com",
                "session": {"sessionId": "session-value", "access_token": "token-value"},
                "items": [{"Password": "hash-value", "otp": "1234", "value": 42}]
            }
        });
        redact(&mut response);
        assert_eq!(response["result"]["status"], "ok");
        assert_eq!(response["result"]["login"], "<masked>");
        assert_eq!(response["result"]["session"]["sessionId"], "<redacted>");
        assert_eq!(response["result"]["session"]["access_token"], "<redacted>");
        assert_eq!(response["result"]["items"][0]["Password"], "<redacted>");
        assert_eq!(response["result"]["items"][0]["otp"], "<redacted>");
        assert_eq!(response["result"]["items"][0]["value"], 42);
    }

    #[test]
    fn chunks_preserve_long_unicode_responses_without_truncation() {
        let message = format!("{}{}", "a".repeat(2_999), "\u{1f510}".repeat(2_000));
        let chunks = log_chunks(&message, 3_000);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 3_000));
        assert_eq!(chunks.concat(), message);
    }
}
