//! Docs-side MCP tools (`search`, `context`, `inventory`, `map`, `status`, `annotate`).
//! The trait is implemented on `DocEngine` in `crate::mcp::docs_engine`. Pagination by
//! token budget happens here, on the unbounded entries the backend returns.
//! [`FakeDocsBackend`] stands in for the unit tests of this module.

use std::path::Path;

use serde_json::{json, Value};

use crate::mcp::args;
use crate::mcp::budget;
use crate::mcp::server::McpServer;
use crate::mcp::tools::{ToolOutcome, ToolSpec};

/// Everything a docs engine must expose to back the MCP docs tools. Every method returns a
/// raw, **unbounded** `serde_json::Value`: budgeting/truncation/pagination is this module's
/// job (§4.3), so a backend never has to know about token budgets.
///
/// `root` is the resolved workspace root; `args` is the tool's raw argument object (after the
/// MCP layer has validated whatever it needs, e.g. a required `query`), so a concrete backend
/// can read whichever extra fields it supports without the trait needing to grow a parameter
/// for each one.
pub trait DocsBackend: Send {
    /// Hybrid search over the docs/specs index (§1.6). Expected shape: `{"results": [...]}`
    /// with each result at least `{path, line, section, text, score}`.
    fn search(&self, root: &Path, args: &Value) -> Value;

    /// Raw material for the `context` tool's co-retrieval, *before* pagination. Expected
    /// shape: `{"entries": [...]}`, each entry carrying at least a `text`/`snippet` field (its
    /// serialised size is what the token budget is measured against).
    fn context(&self, root: &Path, args: &Value) -> Value;

    /// Structured rows of one inventory (or the list of inventories when `kind` is absent).
    fn inventory(&self, root: &Path, args: &Value) -> Value;

    /// One-line-per-entry summary tree of the workspace.
    fn map(&self, root: &Path, args: &Value) -> Value;

    /// Freshness/store/sources/coverage snapshot.
    fn status(&self, root: &Path, args: &Value) -> Value;

    /// Records an agent summary/tags for `path` (source file itself stays untouched).
    fn annotate(&self, root: &Path, args: &Value) -> Value;
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "search",
            description: "Ranked documentation passages with source path and line range. \
                Start here for a question. Filters: tags, source_types, since, path_prefixes. \
                If the payload has `upgrade`, tell the user — the corpus is large and still \
                on the hash embedder; they should plug Ollama (or an OpenAI-compatible server) \
                in ctxq.toml and run `ctxq sync`.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "default": 5},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "source_types": {"type": "array", "items": {"type": "string"}},
                    "since": {"type": "string", "description": "ISO date filter"},
                    "path_prefixes": {"type": "array", "items": {"type": "string"}},
                    "max_chars": {"type": "integer", "default": 0, "description": "0 = unbounded"},
                    "workspace": {"type": "string"},
                },
                "required": ["query"],
            }),
        },
        ToolSpec {
            name: "context",
            description: "Docs and code together, paged inside a token budget. Use when search \
                is not enough and you need both sides. `page` continues where the last call stopped.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "scope": {"type": "string", "enum": ["auto", "docs", "code"], "default": "auto"},
                    "max_tokens": {"type": "integer", "default": 2000},
                    "page": {"type": "integer", "default": 1},
                    "workspace": {"type": "string"},
                },
                "required": ["query"],
            }),
        },
        ToolSpec {
            name: "inventory",
            description: "Exact inventory rows — which port, container, domain, unit. No vector \
                search. Omit `kind` to list known inventories.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string"},
                    "filter": {"type": "string"},
                    "limit": {"type": "integer", "default": 50},
                    "workspace": {"type": "string"},
                },
                "required": [],
            }),
        },
        ToolSpec {
            name: "map",
            description: "One-line-per-entry summary tree of the workspace, optionally rooted \
                at `prefix`.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prefix": {"type": "string"},
                    "depth": {"type": "integer", "default": 2},
                    "limit": {"type": "integer", "default": 200},
                    "workspace": {"type": "string"},
                },
                "required": [],
            }),
        },
        ToolSpec {
            name: "status",
            description: "Freshness, store, sources and coverage snapshot of the workspace. \
                If `upgrade` is set, show it to the user: how to switch from local-hash to \
                Ollama/OpenAI embeddings, and optionally Qdrant.",
            input_schema: json!({
                "type": "object",
                "properties": { "workspace": {"type": "string"} },
                "required": [],
            }),
        },
        ToolSpec {
            name: "annotate",
            description: "Records an agent-authored summary/tags for a file; the source file \
                itself is never modified.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "summary": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "author": {"type": "string", "default": "cli"},
                    "workspace": {"type": "string"},
                },
                "required": ["path"],
            }),
        },
    ]
}

pub fn search(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    let query = match args::require_str(raw_args, "query") {
        Ok(q) => q,
        Err(e) => return ToolOutcome::error(e),
    };
    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    let max_chars = args::opt_u32(raw_args, "max_chars", 0) as usize;

    let mut result = server.docs().search(&ws.root, raw_args);
    let truncated = bound_array_field(&mut result, "results", max_chars);
    let mut out = result;
    if truncated {
        out["truncated"] = json!(true);
    }
    let _ = query; // validated above; forwarded to the backend inside raw_args
    ToolOutcome::ok_json(&out)
}

pub fn context(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    let query = match args::require_str(raw_args, "query") {
        Ok(q) => q,
        Err(e) => return ToolOutcome::error(e),
    };
    let scope = args::opt_str(raw_args, "scope").unwrap_or("auto");
    if !["auto", "docs", "code"].contains(&scope) {
        return ToolOutcome::error(format!(
            "invalid scope: {scope:?}, expected \"auto\", \"docs\" or \"code\""
        ));
    }
    let max_tokens = args::opt_u32(raw_args, "max_tokens", 2000);
    let page = args::opt_u32(raw_args, "page", 1).max(1) as usize;

    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };

    let raw = server.docs().context(&ws.root, raw_args);
    let entries: Vec<Value> = raw
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total_entries = entries.len();

    let max_chars = budget::tokens_to_chars(max_tokens);
    let pages = paginate(&entries, max_chars);
    let total_pages = pages.len().max(1);
    let current_page = page.min(total_pages);
    let page_entries = pages.get(current_page - 1).cloned().unwrap_or_default();
    let has_more = current_page < total_pages;
    let tokens_used = page_entries
        .iter()
        .map(|e| e.to_string().len())
        .sum::<usize>()
        / budget::CHARS_PER_TOKEN;

    let excluded_entities: Vec<Value> = (current_page..total_pages)
        .map(|p| {
            let next = p + 1;
            json!({
                "page": next,
                "count": pages.get(p).map(|v| v.len()).unwrap_or(0),
                "fetch_command": format!("context query={query:?} page={next}"),
            })
        })
        .collect();

    let out = json!({
        "entries": page_entries,
        "ledger": {
            "current_page": current_page,
            "total_pages": total_pages,
            "total_entries": total_entries,
            "has_more": has_more,
            "tokens_used": tokens_used,
            "token_budget": max_tokens,
            "excluded_entities": excluded_entities,
            "next_page_command": if has_more {
                Value::String(format!("context query={query:?} page={}", current_page + 1))
            } else {
                Value::Null
            },
        },
        "truncated": has_more,
    });
    ToolOutcome::ok_json(&out)
}

/// Greedily packs `items` into pages of at most `max_chars` serialised size each (never
/// splitting an item, always keeping at least one item per page). `max_chars == 0` yields a
/// single page with everything.
fn paginate(items: &[Value], max_chars: usize) -> Vec<Vec<Value>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let mut pages = Vec::new();
    let mut remaining = items.to_vec();
    while !remaining.is_empty() {
        let (kept, _, truncated) = budget::bound_values(remaining.clone(), max_chars);
        let n = kept.len().max(1);
        pages.push(remaining[..n].to_vec());
        remaining.drain(..n);
        if !truncated {
            break;
        }
    }
    pages
}

/// Bounds the array at `field` (if present) to `max_chars` of serialised size, in place.
/// Returns whether anything was actually cut. `max_chars == 0` disables bounding.
fn bound_array_field(value: &mut Value, field: &str, max_chars: usize) -> bool {
    if max_chars == 0 {
        return false;
    }
    let Some(array) = value.get(field).and_then(Value::as_array).cloned() else {
        return false;
    };
    let (kept, _, truncated) = budget::bound_values(array, max_chars);
    if let Some(obj) = value.as_object_mut() {
        obj.insert(field.to_string(), Value::Array(kept));
    }
    truncated
}

pub fn inventory(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    ToolOutcome::ok_json(&server.docs().inventory(&ws.root, raw_args))
}

pub fn map(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    ToolOutcome::ok_json(&server.docs().map(&ws.root, raw_args))
}

pub fn status(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    ToolOutcome::ok_json(&server.docs().status(&ws.root, raw_args))
}

pub fn annotate(server: &McpServer, raw_args: &Value) -> ToolOutcome {
    if let Err(e) = args::require_str(raw_args, "path") {
        return ToolOutcome::error(e);
    }
    let ws = match server.workspace(args::opt_str(raw_args, "workspace")) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    ToolOutcome::ok_json(&server.docs().annotate(&ws.root, raw_args))
}

/// Deterministic, in-memory [`DocsBackend`] used only by tests in this crate (`src/mcp/`):
/// `context`/`search` serve a fixed list of entries, `annotate` records every call it receives
/// so tests can assert on it.
#[cfg(test)]
#[derive(Default)]
pub struct FakeDocsBackend {
    pub annotations: std::sync::Mutex<Vec<Value>>,
}

#[cfg(test)]
impl FakeDocsBackend {
    /// `n` entries, each with a `text` field long enough to make token-budget pagination
    /// exercise more than one page in tests.
    fn fixture_entries(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| {
                json!({
                    "source": format!("docs/spec_{i}.md"),
                    "section": format!("Section {i}"),
                    "text": "x".repeat(40),
                })
            })
            .collect()
    }
}

#[cfg(test)]
impl DocsBackend for FakeDocsBackend {
    fn search(&self, _root: &Path, args: &Value) -> Value {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let results: Vec<Value> = Self::fixture_entries(5)
            .into_iter()
            .filter(|_| !query.is_empty())
            .collect();
        json!({ "results": results })
    }

    fn context(&self, _root: &Path, args: &Value) -> Value {
        let n = args.get("entry_count").and_then(Value::as_u64).unwrap_or(5) as usize;
        json!({ "entries": Self::fixture_entries(n) })
    }

    fn inventory(&self, _root: &Path, _args: &Value) -> Value {
        json!({ "inventories": ["sources", "routes"] })
    }

    fn map(&self, _root: &Path, _args: &Value) -> Value {
        json!({ "tree": [{"path": "docs/", "summary": "spec root"}] })
    }

    fn status(&self, _root: &Path, _args: &Value) -> Value {
        json!({ "fresh": true, "sources": 1 })
    }

    fn annotate(&self, _root: &Path, args: &Value) -> Value {
        self.annotations.lock().unwrap().push(args.clone());
        json!({ "status": "ok", "path": args.get("path") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::store::CodeStore;
    use crate::mcp::server::{McpServer, WorkspaceConfig};
    use std::sync::Arc;

    fn test_server() -> McpServer {
        let ws = WorkspaceConfig {
            name: "default".into(),
            root: std::env::current_dir().unwrap(),
            code: CodeStore::in_memory().unwrap(),
        };
        McpServer::new(vec![ws], Arc::new(FakeDocsBackend::default())).unwrap()
    }

    #[test]
    fn search_requires_query() {
        let server = test_server();
        let out = search(&server, &json!({}));
        assert!(out.is_error);
    }

    #[test]
    fn search_returns_backend_results() {
        let server = test_server();
        let out = search(&server, &json!({ "query": "hybrid" }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn search_bounds_results_when_max_chars_given() {
        let server = test_server();
        let out = search(&server, &json!({ "query": "hybrid", "max_chars": 10 }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert!(v["results"].as_array().unwrap().len() < 5);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn context_requires_query() {
        let server = test_server();
        let out = context(&server, &json!({}));
        assert!(out.is_error);
    }

    #[test]
    fn context_rejects_invalid_scope() {
        let server = test_server();
        let out = context(&server, &json!({ "query": "q", "scope": "bogus" }));
        assert!(out.is_error);
    }

    #[test]
    fn context_fits_everything_on_one_page_under_generous_budget() {
        let server = test_server();
        let out = context(&server, &json!({ "query": "q", "max_tokens": 5000 }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["ledger"]["total_pages"], 1);
        assert_eq!(v["ledger"]["has_more"], false);
        assert_eq!(v["entries"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn context_paginates_under_a_tight_budget() {
        let server = test_server();
        // Each fixture entry serialises to well over 40 chars; a tiny budget forces several
        // pages, one entry at a time at minimum.
        let out = context(&server, &json!({ "query": "q", "max_tokens": 15 }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert!(v["ledger"]["total_pages"].as_u64().unwrap() > 1);
        assert_eq!(v["ledger"]["has_more"], true);
        assert!(!v["ledger"]["next_page_command"]
            .as_str()
            .unwrap()
            .is_empty());
        assert!(!v["ledger"]["excluded_entities"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn context_second_page_picks_up_where_first_left_off() {
        let server = test_server();
        let page1 = context(
            &server,
            &json!({ "query": "q", "max_tokens": 15, "page": 1 }),
        );
        let page2 = context(
            &server,
            &json!({ "query": "q", "max_tokens": 15, "page": 2 }),
        );
        let v1: Value = serde_json::from_str(&page1.text).unwrap();
        let v2: Value = serde_json::from_str(&page2.text).unwrap();
        assert_ne!(
            v1["entries"][0]["source"], v2["entries"][0]["source"],
            "page 2 must not repeat page 1's entries"
        );
    }

    #[test]
    fn inventory_map_status_forward_backend_output() {
        let server = test_server();
        assert!(!inventory(&server, &json!({})).is_error);
        assert!(!map(&server, &json!({})).is_error);
        assert!(!status(&server, &json!({})).is_error);
    }

    #[test]
    fn annotate_requires_path_and_forwards_to_backend() {
        let server = test_server();
        let missing = annotate(&server, &json!({}));
        assert!(missing.is_error);

        let ok = annotate(&server, &json!({ "path": "README.md", "summary": "hello" }));
        assert!(!ok.is_error, "{}", ok.text);
        let v: Value = serde_json::from_str(&ok.text).unwrap();
        assert_eq!(v["status"], "ok");
    }

    #[test]
    fn unknown_workspace_is_a_tool_error_for_every_docs_tool() {
        let server = test_server();
        let args = json!({ "workspace": "nope", "query": "q", "path": "x" });
        assert!(search(&server, &args).is_error);
        assert!(context(&server, &args).is_error);
        assert!(inventory(&server, &args).is_error);
        assert!(map(&server, &args).is_error);
        assert!(status(&server, &args).is_error);
        assert!(annotate(&server, &args).is_error);
    }
}
