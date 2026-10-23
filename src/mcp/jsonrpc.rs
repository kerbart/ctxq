//! Wire-level JSON-RPC 2.0 plumbing for the stdio MCP transport: one JSON object per line,
//! no `Content-Length` framing. See `docs/v3/SPEC.md` §4.1.

use serde::Deserialize;
use serde_json::{json, Value};

/// Standard JSON-RPC 2.0 error codes used by this server. Only *protocol* errors (malformed
/// JSON, unknown method, malformed params) use these; a tool failing its own job is reported
/// as a successful `tools/call` response with `isError: true` instead (see `ToolOutcome`).
pub const PARSE_ERROR: i64 = -32700;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// A single incoming JSON-RPC line, loosely parsed: `params` defaults to `Null` rather than
/// failing when absent, and `id` distinguishes a request (`Some(v)` with `v` not `Null`) from
/// a notification (missing key *or* explicit `null`), per the spec's "id absent ou null" rule.
#[derive(Debug, Deserialize)]
pub struct RawRequest {
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub id: Option<Value>,
}

impl RawRequest {
    /// `true` when this message must never receive a response (JSON-RPC notification).
    pub fn is_notification(&self) -> bool {
        matches!(self.id, None | Some(Value::Null))
    }
}

/// Builds a successful JSON-RPC response line for request id `id`.
pub fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Builds a JSON-RPC protocol-error response line. `id` is `Null` when the original id could
/// not be recovered (e.g. the line itself failed to parse).
pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_missing_id_as_notification() {
        let req: RawRequest =
            serde_json::from_str(r#"{"method":"notifications/initialized"}"#).expect("valid json");
        assert!(req.is_notification());
    }

    #[test]
    fn treats_null_id_as_notification() {
        let req: RawRequest =
            serde_json::from_str(r#"{"method":"ping","id":null}"#).expect("valid json");
        assert!(req.is_notification());
    }

    #[test]
    fn treats_present_id_as_request() {
        let req: RawRequest =
            serde_json::from_str(r#"{"method":"ping","id":7}"#).expect("valid json");
        assert!(!req.is_notification());
    }

    #[test]
    fn defaults_missing_params_to_null() {
        let req: RawRequest = serde_json::from_str(r#"{"method":"ping","id":1}"#).unwrap();
        assert_eq!(req.params, Value::Null);
    }
}
