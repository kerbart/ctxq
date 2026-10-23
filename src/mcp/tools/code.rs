//! `code_*` tools: thin MCP wrappers around `crate::code::query` and `crate::code::sync`.

use std::path::Path;
use std::process::Command;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::code::model::Lang;
use crate::code::query::{self, SymbolHit};
use crate::code::store::CodeStore;
use crate::code::sync::{sync_tree, IgnoreRules};
use crate::mcp::args;
use crate::mcp::budget;
use crate::mcp::server::McpServer;
use crate::mcp::tools::{ToolOutcome, ToolSpec};

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "code_symbol",
            description: "Resolves a symbol by name (exact, then qualified, then prefix, then \
                case-insensitive) and returns its signature, doc comment and a bounded source \
                body. Returns a candidate list instead when several symbols match.",
            input_schema: object_schema(
                json!({
                    "name": {"type": "string", "description": "Symbol name, qualified name, or prefix"},
                    "max_tokens": {"type": "integer", "default": 2000},
                    "start_line": {"type": "integer", "description": "Optional body window start (1-based)"},
                    "end_line": {"type": "integer", "description": "Optional body window end (1-based)"},
                    "workspace": {"type": "string"},
                }),
                &["name"],
            ),
        },
        ToolSpec {
            name: "code_callers",
            description: "Lists the callers of a symbol, breadth-first, up to `depth` hops \
                (never revisiting a node).",
            input_schema: call_graph_schema(),
        },
        ToolSpec {
            name: "code_callees",
            description: "Lists the symbols called by a symbol, breadth-first, up to `depth` \
                hops (never revisiting a node).",
            input_schema: call_graph_schema(),
        },
        ToolSpec {
            name: "code_path",
            description: "Shortest call path between two symbols (forward edges only), bounded \
                by `max_depth` hops.",
            input_schema: object_schema(
                json!({
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "max_depth": {"type": "integer", "default": 5},
                    "max_tokens": {"type": "integer", "default": 2000},
                    "workspace": {"type": "string"},
                }),
                &["from", "to"],
            ),
        },
        ToolSpec {
            name: "code_impact",
            description: "Impact of a diff: symbols touched by the changed line ranges, then \
                their transitive callers up to `depth` hops. Pass `diff` (a `git diff -U0` \
                text) directly, or let the tool run `git diff -U0 <base> <head>` itself.",
            input_schema: object_schema(
                json!({
                    "diff": {"type": "string", "description": "Pre-computed `git diff -U0` text"},
                    "base": {"type": "string", "default": "HEAD~1"},
                    "head": {"type": "string", "default": "HEAD"},
                    "depth": {"type": "integer", "default": 3},
                    "max_tokens": {"type": "integer", "default": 3000},
                    "workspace": {"type": "string"},
                }),
                &[],
            ),
        },
        ToolSpec {
            name: "code_sync",
            description: "Reindexes the code graph: full tree resync when `paths` is omitted, \
                or a targeted re-extraction of the given relative paths (created/modified \
                files are re-extracted, missing ones are removed).",
            input_schema: object_schema(
                json!({
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Relative paths to re-extract; omit for a full resync",
                    },
                    "workspace": {"type": "string"},
                }),
                &[],
            ),
        },
    ]
}

fn call_graph_schema() -> Value {
    object_schema(
        json!({
            "name": {"type": "string"},
            "depth": {"type": "integer", "default": 1, "minimum": 1, "maximum": 3},
            "max_tokens": {"type": "integer", "default": 2000},
            "workspace": {"type": "string"},
        }),
        &["name"],
    )
}

fn symbol_summary(hit: &SymbolHit) -> Value {
    json!({
        "symbol": hit.qname,
        "name": hit.name,
        "kind": hit.kind,
        "file": hit.path,
        "line": hit.start_line,
        "reason": "ambigu: plusieurs symboles correspondent",
    })
}

/// Resolves `name` to exactly one symbol, or returns a `ToolOutcome` the caller should return
/// as-is: a hard error when nothing matches, a (non-error) candidate list when several do.
fn resolve_one(store: &CodeStore, name: &str) -> Result<SymbolHit, ToolOutcome> {
    let hits = query::find(store, name)
        .map_err(|e| ToolOutcome::error(format!("symbol search failed: {e}")))?;
    match hits.len() {
        0 => Err(ToolOutcome::error(format!("symbol not found: {name:?}"))),
        1 => Ok(hits.into_iter().next().expect("checked len == 1")),
        _ => Err(ToolOutcome::ok_json(&json!({
            "ambiguous": true,
            "query": name,
            "candidates": hits.iter().map(symbol_summary).collect::<Vec<_>>(),
        }))),
    }
}

fn bounded_body(
    root: &Path,
    hit: &SymbolHit,
    max_tokens: u32,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> Result<(String, Option<usize>, Option<String>), String> {
    let full = query::body(root, hit).map_err(|e| format!("failed to read body: {e}"))?;
    let window_start = start_line.unwrap_or(hit.start_line).max(hit.start_line);
    let window_end = end_line.unwrap_or(hit.end_line).min(hit.end_line);
    let windowed: Vec<&str> = full
        .lines()
        .enumerate()
        .filter(|(i, _)| {
            let line_no = hit.start_line + *i as u32;
            line_no >= window_start && line_no <= window_end
        })
        .map(|(_, l)| l)
        .collect();
    let max_chars = budget::tokens_to_chars(max_tokens);
    let (kept, omitted) = budget::truncate_lines(&windowed, max_chars);
    if omitted > 0 {
        let hint = format!(
            "body truncated: {omitted} line(s) omitted; rerun code_symbol with \
             start_line/end_line or a larger max_tokens"
        );
        Ok((kept, Some(omitted), Some(hint)))
    } else {
        Ok((kept, None, None))
    }
}

pub fn code_symbol(server: &McpServer, args: &Value) -> ToolOutcome {
    let name = match self::args::require_str(args, "name") {
        Ok(n) => n,
        Err(e) => return ToolOutcome::error(e),
    };
    let max_tokens = self::args::opt_u32(args, "max_tokens", 2000);
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let workspace = self::args::opt_str(args, "workspace");

    let ws = match server.workspace(workspace) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    let store = ws.code.lock().expect("code store mutex poisoned");
    let hit = match resolve_one(&store, name) {
        Ok(h) => h,
        Err(outcome) => return outcome,
    };

    let (body_snippet, body_lines_omitted, hint) =
        match bounded_body(&ws.root, &hit, max_tokens, start_line, end_line) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::error(e),
        };

    let mut out = json!({
        "symbol": hit.qname,
        "name": hit.name,
        "kind": hit.kind,
        "lang": hit.lang,
        "file": hit.path,
        "start_line": hit.start_line,
        "end_line": hit.end_line,
        "signature": hit.signature,
        "doc_comment": hit.doc,
        "body_snippet": body_snippet,
    });
    if let Some(n) = body_lines_omitted {
        out["body_lines_omitted"] = json!(n);
        out["hint"] = json!(hint);
    }
    ToolOutcome::ok_json(&out)
}

fn call_graph_tool(server: &McpServer, args: &Value, forward: bool) -> ToolOutcome {
    let name = match self::args::require_str(args, "name") {
        Ok(n) => n,
        Err(e) => return ToolOutcome::error(e),
    };
    let depth = self::args::clamp_depth(self::args::opt_u32(args, "depth", 1), 3);
    let max_tokens = self::args::opt_u32(args, "max_tokens", 2000);
    let workspace = self::args::opt_str(args, "workspace");

    let ws = match server.workspace(workspace) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    let store = ws.code.lock().expect("code store mutex poisoned");
    let hit = match resolve_one(&store, name) {
        Ok(h) => h,
        Err(outcome) => return outcome,
    };

    let graph = if forward {
        query::callees(&store, hit.id, depth)
    } else {
        query::callers(&store, hit.id, depth)
    };
    let graph = match graph {
        Ok(g) => g,
        Err(e) => return ToolOutcome::error(format!("graph traversal failed: {e}")),
    };

    let max_chars = budget::tokens_to_chars(max_tokens);
    let edge_values: Vec<Value> = graph
        .edges
        .iter()
        .map(|e| serde_json::to_value(e).expect("CallEdge always serialises"))
        .collect();
    let (edges, omitted, truncated) = budget::bound_values(edge_values, max_chars);
    let ambiguous: Vec<Value> = graph
        .ambiguous
        .iter()
        .map(|a| serde_json::to_value(a).expect("AmbiguousCall always serialises"))
        .collect();

    let mut out = json!({
        "symbol": hit.qname,
        "direction": if forward { "callees" } else { "callers" },
        "depth": depth,
        "edges": edges,
        "ambiguous_calls": ambiguous,
    });
    if truncated {
        out["truncated"] = json!(true);
        out["edges_omitted"] = json!(omitted);
        out["hint"] = json!(format!(
            "increase max_tokens (current budget: {max_tokens}) to see the remaining {omitted} edge(s)"
        ));
    }
    ToolOutcome::ok_json(&out)
}

pub fn code_callers(server: &McpServer, args: &Value) -> ToolOutcome {
    call_graph_tool(server, args, false)
}

pub fn code_callees(server: &McpServer, args: &Value) -> ToolOutcome {
    call_graph_tool(server, args, true)
}

pub fn code_path(server: &McpServer, args: &Value) -> ToolOutcome {
    let from = match self::args::require_str(args, "from") {
        Ok(n) => n,
        Err(e) => return ToolOutcome::error(e),
    };
    let to = match self::args::require_str(args, "to") {
        Ok(n) => n,
        Err(e) => return ToolOutcome::error(e),
    };
    let max_depth = self::args::opt_u32(args, "max_depth", 5).clamp(1, 10);
    let max_tokens = self::args::opt_u32(args, "max_tokens", 2000);
    let workspace = self::args::opt_str(args, "workspace");

    let ws = match server.workspace(workspace) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    let store = ws.code.lock().expect("code store mutex poisoned");
    let a = match resolve_one(&store, from) {
        Ok(h) => h,
        Err(outcome) => return outcome,
    };
    let b = match resolve_one(&store, to) {
        Ok(h) => h,
        Err(outcome) => return outcome,
    };

    match query::path(&store, a.id, b.id, max_depth) {
        Ok(None) => ToolOutcome::ok_json(&json!({
            "reachable": false,
            "from": a.qname,
            "to": b.qname,
            "max_depth": max_depth,
        })),
        Ok(Some(edges)) => {
            let max_chars = budget::tokens_to_chars(max_tokens);
            let edge_values: Vec<Value> = edges
                .iter()
                .map(|e| serde_json::to_value(e).expect("CallEdge always serialises"))
                .collect();
            let (steps, omitted, truncated) = budget::bound_values(edge_values, max_chars);
            let mut out = json!({
                "reachable": true,
                "from": a.qname,
                "to": b.qname,
                "distance": edges.len(),
                "steps": steps,
            });
            if truncated {
                out["truncated"] = json!(true);
                out["steps_omitted"] = json!(omitted);
            }
            ToolOutcome::ok_json(&out)
        }
        Err(e) => ToolOutcome::error(format!("path search failed: {e}")),
    }
}

fn run_git_diff(root: &Path, base: &str, head: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .arg("-U0")
        .arg(base)
        .arg(head)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    // `git diff` exits 0 (no differences) or 1 (differences found) on success; only >1 is an
    // actual failure (bad ref, not a repo, ...).
    match output.status.code() {
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => Err(format!(
            "git diff -U0 {base} {head} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

pub fn code_impact(server: &McpServer, args: &Value) -> ToolOutcome {
    let depth = self::args::clamp_depth(self::args::opt_u32(args, "depth", 3), 5);
    let max_tokens = self::args::opt_u32(args, "max_tokens", 3000);
    let workspace = self::args::opt_str(args, "workspace");

    let ws = match server.workspace(workspace) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };

    let diff_text = match self::args::opt_str(args, "diff") {
        Some(d) => d.to_string(),
        None => {
            let base = self::args::opt_str(args, "base").unwrap_or("HEAD~1");
            let head = self::args::opt_str(args, "head").unwrap_or("HEAD");
            match run_git_diff(&ws.root, base, head) {
                Ok(t) => t,
                Err(e) => return ToolOutcome::error(e),
            }
        }
    };

    let store = ws.code.lock().expect("code store mutex poisoned");
    let report = match query::impact(&store, &diff_text, depth, None) {
        Ok(r) => r,
        Err(e) => return ToolOutcome::error(format!("impact computation failed: {e}")),
    };

    let max_chars = budget::tokens_to_chars(max_tokens);
    let touched_values: Vec<Value> = report
        .touched
        .iter()
        .map(|h| serde_json::to_value(h).expect("SymbolHit always serialises"))
        .collect();
    let (touched, touched_omitted, touched_truncated) =
        budget::bound_values(touched_values, max_chars / 2);
    let callers_values: Vec<Value> = report
        .callers
        .iter()
        .map(|h| serde_json::to_value(h).expect("SymbolHit always serialises"))
        .collect();
    let (callers, callers_omitted, callers_truncated) =
        budget::bound_values(callers_values, max_chars / 2);

    let mut out = json!({
        "depth": depth,
        "touched": touched,
        "transitive_callers": callers,
        "unresolved_files": report.unresolved_files,
    });
    if touched_truncated || callers_truncated {
        out["truncated"] = json!(true);
        out["touched_omitted"] = json!(touched_omitted);
        out["callers_omitted"] = json!(callers_omitted);
    }
    ToolOutcome::ok_json(&out)
}

pub fn code_sync(server: &McpServer, args: &Value) -> ToolOutcome {
    let workspace = self::args::opt_str(args, "workspace");
    let ws = match server.workspace(workspace) {
        Ok(ws) => ws,
        Err(e) => return ToolOutcome::error(e),
    };
    let paths = self::args::opt_str_array(args, "paths");
    let mut store = ws.code.lock().expect("code store mutex poisoned");

    if paths.is_empty() {
        return match sync_tree(&mut store, &ws.root, &IgnoreRules::default()) {
            Ok(report) => ToolOutcome::ok_json(&json!({ "mode": "full", "report": report })),
            Err(e) => ToolOutcome::error(format!("sync failed: {e}")),
        };
    }

    let mut extracted = 0usize;
    let mut removed = 0usize;
    let mut skipped_unsupported = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for rel in &paths {
        let full_path = ws.root.join(rel);
        if !full_path.exists() {
            match store.remove_file(rel) {
                Ok(()) => removed += 1,
                Err(e) => errors.push(format!("{rel}: deletion failed: {e}")),
            }
            continue;
        }
        let Some(lang) = Lang::for_path(Path::new(rel)) else {
            skipped_unsupported += 1;
            continue;
        };
        match std::fs::read(&full_path) {
            Ok(bytes) => {
                let sha256 = format!("{:x}", Sha256::digest(&bytes));
                let mtime = std::fs::metadata(&full_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let size = bytes.len() as i64;
                let source = String::from_utf8_lossy(&bytes);
                let outline = crate::code::extract::extract(lang, &source);
                match store.upsert_file(rel, lang, mtime, size, &sha256, &outline) {
                    Ok(()) => extracted += 1,
                    Err(e) => errors.push(format!("{rel}: indexing failed: {e}")),
                }
            }
            Err(e) => errors.push(format!("{rel}: lecture impossible: {e}")),
        }
    }

    if let Err(e) = store.resolve_calls() {
        return ToolOutcome::error(format!("call resolution failed: {e}"));
    }

    ToolOutcome::ok_json(&json!({
        "mode": "partial",
        "requested": paths.len(),
        "extracted": extracted,
        "removed": removed,
        "skipped_unsupported_extension": skipped_unsupported,
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::server::{McpServer, WorkspaceConfig};
    use crate::mcp::tools::tests::FakeDocsBackend;
    use std::fs;
    use std::sync::Arc;

    /// `crate::code::extract::extract` is still the other team's stub (always returns an
    /// empty `FileOutline`, see `docs/v3/CODEGRAPH-NOTES.md`), so fixtures used by tools that
    /// read resolved symbols/calls build the graph by hand (`upsert_file`) rather than
    /// relying on a real sync — the same convention `src/code/sync.rs`'s own tests use. The
    /// source file is still written to disk with matching line numbers so `code_symbol`'s
    /// bounded body (which rereads the file) has something real to slice.
    fn server_with_fixture() -> (McpServer, tempfile::TempDir) {
        use crate::code::model::{Call, Symbol, SymbolKind};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let source =
            "fn outer() {\n    inner();\n}\n\nfn inner() {\n    core();\n}\n\nfn core() {}\n";
        fs::write(root.join("a.rs"), source).unwrap();

        let symbol = |name: &str, start: u32, end: u32| Symbol {
            name: name.to_string(),
            qname: name.to_string(),
            kind: SymbolKind::Function,
            lang: Lang::Rust,
            start_line: start,
            end_line: end,
            signature: format!("fn {name}()"),
            doc: String::new(),
            parent_qname: None,
        };
        let outline = crate::code::model::FileOutline {
            symbols: vec![
                symbol("outer", 1, 3),
                symbol("inner", 5, 7),
                symbol("core", 9, 9),
            ],
            calls: vec![
                Call {
                    caller_qname: "outer".into(),
                    callee: "inner".into(),
                    line: 2,
                },
                Call {
                    caller_qname: "inner".into(),
                    callee: "core".into(),
                    line: 6,
                },
            ],
            imports: vec![],
        };

        let mut code = CodeStore::in_memory().unwrap();
        let disk_mtime = fs::metadata(root.join("a.rs"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let disk_sha = format!("{:x}", Sha256::digest(source.as_bytes()));
        code.upsert_file(
            "a.rs",
            Lang::Rust,
            disk_mtime,
            source.len() as i64,
            &disk_sha,
            &outline,
        )
        .unwrap();
        code.resolve_calls().unwrap();
        // Stamp the current extractor version so a later `code_sync` full resync doesn't
        // treat this hand-built fixture as stale and force-reextract it with the stub.
        code.set_extractor_version(crate::code::model::EXTRACTOR_VERSION)
            .unwrap();

        let ws = WorkspaceConfig {
            name: "default".into(),
            root: root.to_path_buf(),
            code,
        };
        let server = McpServer::new(vec![ws], Arc::new(FakeDocsBackend::default())).unwrap();
        (server, dir)
    }

    #[test]
    fn code_symbol_finds_unique_match_with_bounded_body() {
        let (server, _dir) = server_with_fixture();
        let out = code_symbol(&server, &json!({ "name": "outer" }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["symbol"], "outer");
        assert!(v["body_snippet"].as_str().unwrap().contains("inner()"));
    }

    #[test]
    fn code_symbol_truncates_body_under_tiny_token_budget() {
        let (server, _dir) = server_with_fixture();
        let out = code_symbol(&server, &json!({ "name": "outer", "max_tokens": 1 }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert!(v["body_lines_omitted"].as_u64().unwrap() > 0);
        assert!(v["hint"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn code_symbol_reports_not_found() {
        let (server, _dir) = server_with_fixture();
        let out = code_symbol(&server, &json!({ "name": "does_not_exist" }));
        assert!(out.is_error);
        assert!(out.text.contains("not found"));
    }

    #[test]
    fn code_symbol_requires_name_argument() {
        let (server, _dir) = server_with_fixture();
        let out = code_symbol(&server, &json!({}));
        assert!(out.is_error);
    }

    #[test]
    fn code_callees_follows_the_call_chain() {
        let (server, _dir) = server_with_fixture();
        let out = code_callees(&server, &json!({ "name": "outer", "depth": 2 }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        let callees: Vec<String> = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["callee"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(callees.contains(&"inner".to_string()));
        assert!(callees.contains(&"core".to_string()));
    }

    #[test]
    fn code_callers_follows_the_reverse_chain() {
        let (server, _dir) = server_with_fixture();
        let out = code_callers(&server, &json!({ "name": "core" }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        let callers: Vec<String> = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["caller"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(callers.contains(&"inner".to_string()));
    }

    #[test]
    fn code_path_finds_shortest_path() {
        let (server, _dir) = server_with_fixture();
        let out = code_path(&server, &json!({ "from": "outer", "to": "core" }));
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["reachable"], true);
        assert_eq!(v["distance"], 2);
    }

    #[test]
    fn code_path_reports_unreachable() {
        let (server, _dir) = server_with_fixture();
        let out = code_path(
            &server,
            &json!({ "from": "core", "to": "outer", "max_depth": 3 }),
        );
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["reachable"], false);
    }

    #[test]
    fn code_impact_resolves_touched_symbols_from_explicit_diff() {
        let (server, _dir) = server_with_fixture();
        // `core` spans line 9 in the fixture source; this hunk marks line 9 as added.
        let diff = "+++ b/a.rs\n@@ -1,0 +9,1 @@\n+fn core() {}\n";
        let out = code_impact(&server, &json!({ "diff": diff, "depth": 2 }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        let touched: Vec<String> = v["touched"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap().to_string())
            .collect();
        assert!(touched.contains(&"core".to_string()));
        let callers: Vec<String> = v["transitive_callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap().to_string())
            .collect();
        assert!(callers.contains(&"inner".to_string()));
    }

    #[test]
    fn code_sync_full_resync_picks_up_new_files_without_touching_unchanged_ones() {
        let (server, dir) = server_with_fixture();
        fs::write(dir.path().join("b.rs"), "fn fresh() {}\n").unwrap();
        let out = code_sync(&server, &json!({}));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["mode"], "full");
        // a.rs's stored mtime/sha match disk (fixture built them for real): only b.rs moves.
        assert_eq!(v["report"]["extracted"], 1);
        assert_eq!(v["report"]["skipped_unchanged"], 1);

        // a.rs's hand-built symbols (extract() is still the other team's empty stub) must
        // have survived untouched.
        let still_there = code_symbol(&server, &json!({ "name": "outer" }));
        assert!(!still_there.is_error, "{}", still_there.text);
    }

    #[test]
    fn code_sync_partial_reextracts_the_given_path_with_the_stub_extractor() {
        let (server, dir) = server_with_fixture();
        fs::write(dir.path().join("a.rs"), "fn changed() {}\n").unwrap();
        let out = code_sync(&server, &json!({ "paths": ["a.rs"] }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["mode"], "partial");
        assert_eq!(v["extracted"], 1);

        // `extract()` is still an empty stub: re-extraction replaces the hand-built symbols
        // with nothing, which still demonstrates the wiring (read -> extract -> upsert) is
        // correct even though the extractor itself has no behaviour yet.
        let gone = code_symbol(&server, &json!({ "name": "outer" }));
        assert!(gone.is_error);
    }

    #[test]
    fn code_sync_partial_removes_deleted_files() {
        let (server, dir) = server_with_fixture();
        fs::remove_file(dir.path().join("a.rs")).unwrap();
        let out = code_sync(&server, &json!({ "paths": ["a.rs"] }));
        assert!(!out.is_error, "{}", out.text);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["removed"], 1);

        let missing = code_symbol(&server, &json!({ "name": "outer" }));
        assert!(missing.is_error);
    }

    #[test]
    fn unknown_workspace_name_is_a_tool_error() {
        let (server, _dir) = server_with_fixture();
        let out = code_symbol(&server, &json!({ "name": "outer", "workspace": "nope" }));
        assert!(out.is_error);
        assert!(out.text.contains("default"));
    }
}
