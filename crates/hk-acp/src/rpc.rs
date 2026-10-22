//! JSON-RPC 2.0 framing for ACP: one JSON object per line (newline-delimited, no embedded
//! newlines), UTF-8, over the agent's stdio.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Standard JSON-RPC error codes we emit.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// ACP-specific: resource (file) not found.
    pub const RESOURCE_NOT_FOUND: i64 = -32002;
}

/// JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL_ERROR, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(code::METHOD_NOT_FOUND, format!("method not supported by this client: {method}"))
    }
}

/// One decoded line.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// Decode one line. Requests and notifications are told apart by `id`; a response has no
/// `method`.
pub fn decode(line: &str) -> Result<Message, RpcError> {
    let raw: Raw = serde_json::from_str(line).map_err(|e| RpcError::new(code::PARSE_ERROR, e.to_string()))?;
    let params = raw.params.unwrap_or(Value::Null);
    match (raw.method, raw.id) {
        (Some(method), Some(id)) if !id.is_null() => Ok(Message::Request { id, method, params }),
        (Some(method), _) => Ok(Message::Notification { method, params }),
        (None, Some(id)) => {
            let result = match raw.error {
                Some(e) => Err(e),
                None => Ok(raw.result.unwrap_or(Value::Null)),
            };
            Ok(Message::Response { id, result })
        }
        (None, None) => Err(RpcError::new(code::PARSE_ERROR, "message has neither method nor id")),
    }
}

/// Encode without the trailing newline (`serde_json` never emits raw newlines).
pub fn encode_request(id: u64, method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn encode_notification(method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

pub fn encode_response(id: &Value, result: &Result<Value, RpcError>) -> String {
    match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let line = encode_request(7, "session/new", &json!({"cwd": "/w"}));
        assert!(!line.contains('\n'));
        assert_eq!(
            decode(&line).unwrap(),
            Message::Request {
                id: json!(7),
                method: "session/new".into(),
                params: json!({"cwd": "/w"})
            }
        );
    }

    #[test]
    fn notification_has_no_id() {
        let line = encode_notification("session/cancel", &json!({"sessionId": "s"}));
        assert!(matches!(decode(&line).unwrap(), Message::Notification { .. }));
    }

    #[test]
    fn embedded_newline_in_string_is_escaped() {
        let line = encode_notification("x", &json!({"t": "a\nb"}));
        assert!(!line.contains('\n'));
        let Message::Notification { params, .. } = decode(&line).unwrap() else {
            panic!("not a notification")
        };
        assert_eq!(params["t"], "a\nb");
    }

    #[test]
    fn response_ok_null_and_error() {
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap(),
            Message::Response {
                id: json!(1),
                result: Ok(Value::Null)
            }
        );
        let Message::Response { result, .. } =
            decode(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nope"}}"#).unwrap()
        else {
            panic!("not a response")
        };
        assert_eq!(result.unwrap_err().code, code::METHOD_NOT_FOUND);
    }

    #[test]
    fn string_ids_and_garbage() {
        let Message::Request { id, .. } = decode(r#"{"jsonrpc":"2.0","id":"a-1","method":"m"}"#).unwrap() else {
            panic!("not a request")
        };
        assert_eq!(id, json!("a-1"));
        assert_eq!(decode("not json").unwrap_err().code, code::PARSE_ERROR);
        assert!(decode("{}").is_err());
    }

    #[test]
    fn response_encoding_echoes_id() {
        let line = encode_response(&json!("x"), &Err(RpcError::method_not_found("terminal/create")));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "x");
        assert_eq!(v["error"]["code"], code::METHOD_NOT_FOUND);
    }
}
