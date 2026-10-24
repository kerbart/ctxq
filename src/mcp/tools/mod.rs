//! Tool registry and dispatch: turns a `tools/call` into a bounded text result. Each concrete
//! tool lives in [`code`] (backed by `crate::code::query`) or [`docs`] (backed by the
//! [`DocsBackend`] trait, which the docs team wires to `DocEngine`).

pub mod code;
pub mod docs;

use serde_json::{json, Value};

use crate::mcp::server::McpServer;

pub use docs::DocsBackend;

#[cfg(test)]
pub mod tests {
    pub use super::docs::FakeDocsBackend;
}

/// Protocol-level description of a tool, as returned by `tools/list`.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// Result of running one tool: bounded text plus the `isError` flag. Never a JSON-RPC
/// protocol error — "invalid business parameter, symbol not found…" are reported here,
/// not via `error_response` (§4.1).
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn ok_json(value: &Value) -> Self {
        Self::ok(serde_json::to_string_pretty(value).unwrap_or_else(|e| e.to_string()))
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    /// The `tools/call` MCP result envelope: `{"content": [...], "isError": bool}`.
    pub fn into_response(self) -> Value {
        json!({
            "content": [{ "type": "text", "text": self.text }],
            "isError": self.is_error,
        })
    }
}

/// Every tool this server exposes. Docs first: that is what an agent should
/// try before walking the tree. Code-graph tools follow.
pub fn registry() -> Vec<ToolSpec> {
    let mut specs = docs::tool_specs();
    specs.extend(code::tool_specs());
    specs
}

/// Short text handed back from `initialize` to orient the client.
pub fn instructions() -> String {
    "Start with inventory (exact rows: port, container, domain), search (ranked \
     passages), or status (is the index fresh?). Use context when you need docs \
     and code in one budgeted page. Code graph: code_symbol, then code_callers / \
     code_callees / code_path / code_impact. After edits, code_sync. Leave notes \
     with annotate. Every tool accepts optional \"workspace\". If search or status \
     returns `upgrade`, tell the user — do not ignore it: the index is still on \
     the hash embedder and a neural backend (Ollama / OpenAI-compatible) would \
     help on this corpus."
        .to_string()
}

/// Dispatches `tools/call` by name. Unknown tool names are a *tool* error (`isError: true`),
/// not a JSON-RPC protocol error, per §4.1.
pub fn call(server: &McpServer, name: &str, arguments: &Value) -> ToolOutcome {
    match name {
        "code_symbol" => code::code_symbol(server, arguments),
        "code_callers" => code::code_callers(server, arguments),
        "code_callees" => code::code_callees(server, arguments),
        "code_path" => code::code_path(server, arguments),
        "code_impact" => code::code_impact(server, arguments),
        "code_sync" => code::code_sync(server, arguments),
        "search" => docs::search(server, arguments),
        "context" => docs::context(server, arguments),
        "inventory" => docs::inventory(server, arguments),
        "map" => docs::map(server, arguments),
        "status" => docs::status(server, arguments),
        "annotate" => docs::annotate(server, arguments),
        other => ToolOutcome::error(format!(
            "unknown tool: {other:?}. Available tools: {}",
            registry()
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests_mod {
    use super::*;

    #[test]
    fn registry_has_exactly_the_expected_tool_names() {
        let names: Vec<&str> = registry().iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "search",
                "context",
                "inventory",
                "map",
                "status",
                "annotate",
                "code_symbol",
                "code_callers",
                "code_callees",
                "code_path",
                "code_impact",
                "code_sync",
            ]
        );
    }

    #[test]
    fn every_tool_has_a_non_empty_description_and_object_schema() {
        for spec in registry() {
            assert!(
                !spec.description.is_empty(),
                "{} has no description",
                spec.name
            );
            assert_eq!(spec.input_schema["type"], "object", "{} schema", spec.name);
        }
    }
}
