//! stdio JSON-RPC 2.0 server loop and workspace registry. §4.1, §4.2.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::code::store::CodeStore;
use crate::mcp::jsonrpc::{self, RawRequest};
use crate::mcp::tools::{self, DocsBackend};

/// Protocol versions this server understands, oldest first. The last entry is the fallback
/// offered when a client requests a version we don't recognise.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// One workspace as handed to [`McpServer::new`]: a name, its filesystem root (used to
/// resolve relative paths and to run `git diff`), and an already-opened code graph store.
/// Opening/locating the underlying database file is a wiring concern left to the caller
/// (the `serve` CLI command), not to this module.
pub struct WorkspaceConfig {
    pub name: String,
    pub root: PathBuf,
    pub code: CodeStore,
}

/// A registered workspace, ready to be queried by tool handlers.
pub struct WorkspaceEntry {
    pub name: String,
    pub root: PathBuf,
    pub code: Mutex<CodeStore>,
}

/// JSON-RPC 2.0 stdio server: owns the workspace registry and the docs backend, and drives
/// the read-dispatch-write loop. Stateless beyond that (no session/auth concept).
pub struct McpServer {
    workspaces: Vec<Arc<WorkspaceEntry>>,
    docs: Arc<dyn DocsBackend>,
}

impl McpServer {
    /// `workspaces` must be non-empty; the first one is the default (§4.2, §3 `--workspace`).
    pub fn new(
        workspaces: Vec<WorkspaceConfig>,
        docs: Arc<dyn DocsBackend>,
    ) -> anyhow::Result<Self> {
        if workspaces.is_empty() {
            anyhow::bail!("at least one workspace is required");
        }
        let workspaces = workspaces
            .into_iter()
            .map(|w| {
                Arc::new(WorkspaceEntry {
                    name: w.name,
                    root: w.root,
                    code: Mutex::new(w.code),
                })
            })
            .collect();
        Ok(Self { workspaces, docs })
    }

    pub fn docs(&self) -> &Arc<dyn DocsBackend> {
        &self.docs
    }

    /// Names of every registered workspace, in registration order (first = default).
    pub fn workspace_names(&self) -> Vec<&str> {
        self.workspaces.iter().map(|w| w.name.as_str()).collect()
    }

    /// Resolves the `workspace` tool argument (§4.2): `None`/empty -> default (first
    /// registered); otherwise an exact name match, or an explicit error listing the
    /// available names.
    pub fn workspace(&self, requested: Option<&str>) -> Result<&Arc<WorkspaceEntry>, String> {
        match requested.filter(|s| !s.is_empty()) {
            None => Ok(&self.workspaces[0]),
            Some(name) => self
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .ok_or_else(|| {
                    format!(
                        "unknown workspace: {name:?}. Available workspaces: {}",
                        self.workspace_names().join(", ")
                    )
                }),
        }
    }

    /// Reads one JSON-RPC message per line from `input`, dispatches it, and writes any
    /// response (also one line) to `output`, until EOF. Mirrors the "Cursor in-memory" test
    /// double: any `BufRead`/`Write` pair works, stdio or `io::Cursor<Vec<u8>>` alike.
    pub fn serve<R: BufRead, W: Write>(&self, mut input: R, mut output: W) -> io::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = input.read_line(&mut line)?;
            if bytes_read == 0 {
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(trimmed) {
                writeln!(output, "{response}")?;
                output.flush()?;
            }
        }
    }

    /// Dispatches a single already-trimmed JSON-RPC line. Returns `None` for notifications
    /// (no response ever sent) or when a notification's own JSON fails to parse.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let request: RawRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                // We cannot recover an id from unparseable JSON; per JSON-RPC 2.0 this is
                // reported with id = null.
                let resp = jsonrpc::error_response(
                    Value::Null,
                    jsonrpc::PARSE_ERROR,
                    format!("parse error: {e}"),
                );
                return Some(resp.to_string());
            }
        };

        if request.is_notification() {
            self.handle_notification(&request.method, &request.params);
            return None;
        }

        let id = request.id.clone().unwrap_or(Value::Null);
        let response = self.handle_request(&request.method, &request.params, id);
        Some(response.to_string())
    }

    fn handle_notification(&self, method: &str, _params: &Value) {
        match method {
            "notifications/initialized" => {
                tracing::debug!("client reported initialized");
            }
            other => tracing::debug!(method = other, "unhandled notification"),
        }
    }

    fn handle_request(&self, method: &str, params: &Value, id: Value) -> Value {
        match method {
            "initialize" => jsonrpc::ok_response(id, self.handle_initialize(params)),
            "ping" => jsonrpc::ok_response(id, json!({})),
            "tools/list" => jsonrpc::ok_response(id, self.handle_tools_list()),
            "tools/call" => match self.handle_tools_call(params) {
                Ok(result) => jsonrpc::ok_response(id, result),
                Err(message) => jsonrpc::error_response(id, jsonrpc::INVALID_PARAMS, message),
            },
            other => jsonrpc::error_response(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            ),
        }
    }

    fn handle_initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let protocol_version = match requested {
            Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
            _ => SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS.len() - 1],
        };
        json!({
            "protocolVersion": protocol_version,
            "serverInfo": { "name": "ctxq", "version": crate::VERSION },
            "capabilities": { "tools": { "listChanged": false } },
            "instructions": tools::instructions(),
        })
    }

    fn handle_tools_list(&self) -> Value {
        let tools: Vec<Value> = tools::registry()
            .into_iter()
            .map(|spec| {
                json!({
                    "name": spec.name,
                    "description": spec.description,
                    "inputSchema": spec.input_schema,
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    fn handle_tools_call(&self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("tools/call requires a string \"name\" parameter")?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let outcome = tools::call(self, name, &arguments);
        Ok(outcome.into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::tests::FakeDocsBackend;
    use std::io::Cursor;

    fn test_server() -> McpServer {
        let code = CodeStore::in_memory().unwrap();
        let ws = WorkspaceConfig {
            name: "default".into(),
            root: PathBuf::from("."),
            code,
        };
        McpServer::new(vec![ws], Arc::new(FakeDocsBackend::default())).unwrap()
    }

    fn roundtrip(server: &McpServer, request: &str) -> Option<Value> {
        server
            .handle_line(request)
            .map(|s| serde_json::from_str(&s).unwrap())
    }

    #[test]
    fn initialize_negotiates_known_version() {
        let server = test_server();
        let resp = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "ctxq");
        let instr = resp["result"]["instructions"].as_str().unwrap_or("");
        assert!(instr.contains("inventory"), "{instr}");
        assert!(instr.contains("search"), "{instr}");
        assert_eq!(
            resp["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        assert!(!resp["result"]["instructions"].as_str().unwrap().is_empty());
    }

    #[test]
    fn initialize_falls_back_on_unknown_version() {
        let server = test_server();
        let resp = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(
            resp["result"]["protocolVersion"],
            *SUPPORTED_PROTOCOL_VERSIONS.last().unwrap()
        );
    }

    #[test]
    fn notifications_initialized_gets_no_response() {
        let server = test_server();
        let resp = server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(resp.is_none());
    }

    #[test]
    fn notification_with_explicit_null_id_also_gets_no_response() {
        let server = test_server();
        let resp = server.handle_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
        assert!(resp.is_none());
    }

    #[test]
    fn ping_returns_empty_object() {
        let server = test_server();
        let resp = roundtrip(&server, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).unwrap();
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn unknown_method_is_a_protocol_error() {
        let server = test_server();
        let resp = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":3,"method":"nonexistent"}"#,
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn malformed_json_is_a_parse_error_with_null_id() {
        let server = test_server();
        let resp = roundtrip(&server, "{not json").unwrap();
        assert_eq!(resp["error"]["code"], jsonrpc::PARSE_ERROR);
        assert_eq!(resp["id"], Value::Null);
    }

    #[test]
    fn tools_list_exposes_the_local_tool_names() {
        let server = test_server();
        let resp = roundtrip(&server, r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#).unwrap();
        let names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "code_symbol",
            "code_callers",
            "code_callees",
            "code_path",
            "code_impact",
            "code_sync",
            "search",
            "context",
            "inventory",
            "map",
            "status",
            "annotate",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        assert!(!names
            .iter()
            .any(|n| n == "explore_code" || n == "get_context"));
    }

    #[test]
    fn tools_call_unknown_name_is_tool_error_not_protocol_error() {
        let server = test_server();
        let resp = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        )
        .unwrap();
        assert!(resp.get("error").is_none());
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn tools_call_missing_name_is_invalid_params() {
        let server = test_server();
        let resp = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}"#,
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], jsonrpc::INVALID_PARAMS);
    }

    #[test]
    fn unknown_workspace_name_lists_available_ones() {
        let server = test_server();
        let err = match server.workspace(Some("nope")) {
            Err(e) => e,
            Ok(_) => panic!("expected an error for an unknown workspace name"),
        };
        assert!(err.contains("default"));
    }

    #[test]
    fn serve_loop_handles_several_lines_over_an_in_memory_cursor() {
        let server = test_server();
        let input = Cursor::new(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n"
                .to_vec(),
        );
        let mut output = Vec::new();
        server.serve(input, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // The notification produced no line: two pings in, two responses out.
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
    }
}
