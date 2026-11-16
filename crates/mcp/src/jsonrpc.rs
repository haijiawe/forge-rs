//! JSON-RPC 2.0 framing shared by both transports.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::Error;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireMessage {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: Option<String>,
    pub result: Option<Value>,
    pub error: Option<WireError>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireError {
    pub code: i64,
    pub message: String,
}

pub(crate) const CODE_METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const SERVER_REQUEST_MESSAGE: &str =
    "MCP server requests are not supported by this client";

/// Accept both numeric and quoted-numeric ids; servers in the wild use both.
pub(crate) fn parse_id(id: Option<&Value>) -> Option<u64> {
    match id? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn object(method: &str, id: Option<u64>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    if let Some(id) = id {
        map.insert("id".to_string(), Value::from(id));
    }
    map.insert("method".to_string(), Value::String(method.to_string()));
    map
}

pub(crate) fn encode_request(
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<Vec<u8>, Error> {
    let mut map = object(method, Some(id));
    if let Some(params) = params {
        map.insert("params".to_string(), params);
    }
    serde_json::to_vec(&Value::Object(map)).map_err(Error::from)
}

pub(crate) fn encode_notification(method: &str, params: Option<Value>) -> Result<Vec<u8>, Error> {
    let mut map = object(method, None);
    if let Some(params) = params {
        map.insert("params".to_string(), params);
    }
    serde_json::to_vec(&Value::Object(map)).map_err(Error::from)
}

/// Reply to a server-initiated request. `id` is echoed verbatim (numbers stay
/// numbers, quoted strings stay quoted) because that is what the server sent.
pub(crate) fn encode_method_not_found(id: &Value) -> Result<Vec<u8>, Error> {
    let mut map = Map::new();
    map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    map.insert("id".to_string(), id.clone());
    let mut error = Map::new();
    error.insert("code".to_string(), Value::from(CODE_METHOD_NOT_FOUND));
    error.insert(
        "message".to_string(),
        Value::String(SERVER_REQUEST_MESSAGE.to_string()),
    );
    map.insert("error".to_string(), Value::Object(error));
    serde_json::to_vec(&Value::Object(map)).map_err(Error::from)
}

pub(crate) fn parse_message(line: &[u8]) -> Result<WireMessage, Error> {
    let message: WireMessage = serde_json::from_slice(line)
        .map_err(|e| Error::protocol(format!("invalid JSON-RPC message: {e}")))?;
    if let Some(version) = &message.jsonrpc {
        if version != "2.0" {
            return Err(Error::protocol(format!(
                "unexpected JSON-RPC version {version:?}"
            )));
        }
    }
    Ok(message)
}
