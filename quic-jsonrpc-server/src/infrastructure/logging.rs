//! Payload logging helpers: truncation and redaction of secrets / PII.

use serde_json::Value;

pub const PAYLOAD_LOG_LIMIT: usize = 1024;

/// JSON keys whose values are never written to the log (compared case-insensitively, `_`/`-`
/// ignored, so `access_token`, `accessToken` and `Access-Token` all match).
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "newpassword",
    "oldpassword",
    "pin",
    "otp",
    "token",
    "accesstoken",
    "refreshtoken",
    "sessionid",
    "authorization",
    "secret",
    "apikey",
    "name",
];

/// Keys holding personal data that are masked rather than dropped (last 4 chars kept).
const MASKED_KEYS: &[&str] = &["phone", "msisdn", "email", "login"];

pub fn truncate_for_log(input: &str) -> String {
    if input.chars().count() <= PAYLOAD_LOG_LIMIT {
        return input.to_string();
    }
    let truncated: String = input.chars().take(PAYLOAD_LOG_LIMIT).collect();
    format!("{truncated}...<truncated>")
}

/// Parses the payload as JSON, redacts credentials / PII and truncates. Malformed input is
/// omitted because it cannot be reliably redacted.
pub fn sanitize_payload_for_log(input: &str) -> String {
    match serde_json::from_str::<Value>(input) {
        Ok(mut value) => {
            redact_sensitive_fields(&mut value);
            truncate_for_log(&value.to_string())
        }
        Err(_) => "<unparseable payload omitted>".to_string(),
    }
}

/// Keeps only the last 4 characters of an identifier (`+380991112233` → `***2233`).
pub fn mask_identifier(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "***".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("***{tail}")
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_' && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn redact_sensitive_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                let normalized = normalize_key(key);
                if SENSITIVE_KEYS.contains(&normalized.as_str()) {
                    *inner = Value::String("<redacted>".to_string());
                } else if MASKED_KEYS.contains(&normalized.as_str()) {
                    if let Some(s) = inner.as_str() {
                        *inner = Value::String(mask_identifier(s));
                    }
                } else {
                    redact_sensitive_fields(inner);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_sensitive_fields),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_secrets_and_masks_pii() {
        let payload = json!({
            "login": "driver@example.com",
            "phone": "+380991112233",
            "password": "secret",
            "device": { "deviceId": "dev-1" },
            "session": { "sessionId": "sess-abc", "access_token": "tok", "Refresh-Token": "r" },
            "list": [{ "token": "x" }]
        })
        .to_string();
        let out: Value = serde_json::from_str(&sanitize_payload_for_log(&payload)).unwrap();
        assert_eq!(out["phone"], json!("***2233"));
        assert_eq!(out["password"], json!("<redacted>"));
        assert_eq!(out["login"], json!("***.com"));
        assert_eq!(out["device"]["deviceId"], json!("dev-1"));
        assert_eq!(out["session"]["sessionId"], json!("<redacted>"));
        assert_eq!(out["session"]["access_token"], json!("<redacted>"));
        assert_eq!(out["session"]["Refresh-Token"], json!("<redacted>"));
        assert_eq!(out["list"][0]["token"], json!("<redacted>"));
    }

    #[test]
    fn masks_short_identifiers_fully() {
        assert_eq!(mask_identifier("1234"), "***");
        assert_eq!(mask_identifier("12345"), "***2345");
    }

    #[test]
    fn registration_names_are_fully_redacted_even_in_nested_or_malformed_payloads() {
        let input = json!({"params": {
            "name": "Тест Водій",
            "email": "test@example.com",
            "password": "5f4dcc3b5aa765d61d8327deb882cf99",
            "nested": [{"Name": "Another Driver"}]
        }})
        .to_string();
        let logged = sanitize_payload_for_log(&input);
        assert!(!logged.contains("Тест"));
        assert!(!logged.contains("Another"));
        let value: Value = serde_json::from_str(&logged).unwrap();
        assert_eq!(value["params"]["name"], "<redacted>");
        assert_eq!(value["params"]["nested"][0]["Name"], "<redacted>");
        assert_eq!(
            sanitize_payload_for_log("{\"name\":\"Test Driver\""),
            "<unparseable payload omitted>"
        );
    }
}
