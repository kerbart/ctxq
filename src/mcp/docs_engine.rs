//! Real documentation backend for the MCP server.
//!
//! This is the only adapter between the agent-facing/MCP API and [`DocEngine`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::cli::config::ProjectPaths;
use crate::code::query::{self, DocMentions, DocRef};
use crate::code::store::CodeStore;
use crate::core::agent::{self, SearchArgs};
use crate::core::docmeta::SearchFilters;
use crate::core::hybrid::DocEngine;
use crate::mcp::tools::DocsBackend;

fn error_value(error: impl std::fmt::Display) -> Value {
    json!({ "error": error.to_string() })
}

fn string_array(args: &Value, name: &str) -> Vec<String> {
    args.get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The agent search payload uses `source` / `lines` / `title`. The docs tool
/// contract also asks for `path` / `line` / `section`. Both stay.
fn with_search_aliases(mut value: Value) -> Value {
    let Some(results) = value.get_mut("results").and_then(Value::as_array_mut) else {
        return value;
    };
    for entry in results {
        let source = entry.get("source").cloned().unwrap_or(Value::Null);
        let line = entry
            .get("lines")
            .and_then(Value::as_array)
            .and_then(|lines| lines.first())
            .cloned()
            .unwrap_or(Value::Null);
        let section = entry.get("title").cloned().unwrap_or(Value::Null);
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("path".into(), source);
            obj.insert("line".into(), line);
            obj.insert("section".into(), section);
        }
    }
    value
}

fn search_args(args: &Value) -> SearchArgs {
    SearchArgs {
        query: args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        limit: args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize,
        filters: SearchFilters {
            tags: string_array(args, "tags"),
            source_types: string_array(args, "source_types"),
            since: args.get("since").and_then(Value::as_str).map(str::to_owned),
            path_prefixes: string_array(args, "path_prefixes"),
        },
        max_chars: args.get("max_chars").and_then(Value::as_u64).unwrap_or(0) as usize,
    }
}

fn code_entries(root: &Path, query_text: &str, depth: u32) -> anyhow::Result<Vec<Value>> {
    let db = ProjectPaths::discover(Some(root))?.docs_db;
    let store = CodeStore::open(db)?;
    let mut hits = Vec::new();
    let mut ids = HashSet::new();
    let candidates = std::iter::once(query_text).chain(
        query_text
            .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | ':' | '.')))
            .filter(|part| part.len() > 1),
    );
    for candidate in candidates {
        for hit in query::find(&store, candidate)? {
            if ids.insert(hit.id) {
                hits.push(hit);
            }
        }
    }

    hits.into_iter()
        .take(12)
        .map(|hit| {
            let callers = query::callers(&store, hit.id, depth)?;
            let callees = query::callees(&store, hit.id, depth)?;
            let body = query::body(root, &hit).unwrap_or_default();
            let snippet = body.lines().take(40).collect::<Vec<_>>().join("\n");
            Ok(json!({
                "type": "code_symbol",
                "symbol": hit.qname,
                "file": hit.path,
                "line": hit.start_line,
                "kind": hit.kind,
                "signature": hit.signature,
                "doc_comment": hit.doc,
                "snippet": snippet,
                "text": snippet,
                "callers": callers.edges,
                "callees": callees.edges,
            }))
        })
        .collect()
}

impl DocsBackend for DocEngine {
    fn search(&self, root: &Path, args: &Value) -> Value {
        match agent::search(self, root, &search_args(args)) {
            Ok(value) => with_search_aliases(value),
            Err(error) => error_value(error),
        }
    }

    fn context(&self, root: &Path, args: &Value) -> Value {
        let query_text = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let scope = args.get("scope").and_then(Value::as_str).unwrap_or("auto");
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, 3) as u32;
        let mut entries = Vec::new();

        if scope != "code" {
            let mut doc_args = search_args(args);
            doc_args.limit = 12;
            match agent::search(self, root, &doc_args) {
                Ok(value) => {
                    entries.extend(
                        value["results"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(|mut entry| {
                                entry["type"] = json!("specification");
                                entry
                            }),
                    );
                }
                Err(error) => return error_value(error),
            }
        }
        if scope != "docs" {
            match code_entries(root, query_text, depth) {
                Ok(code) => entries.extend(code),
                Err(error) => return error_value(error),
            }
        }
        json!({ "entries": entries })
    }

    fn inventory(&self, root: &Path, args: &Value) -> Value {
        agent::inventory(
            self,
            root,
            args.get("kind").and_then(Value::as_str),
            args.get("filter").and_then(Value::as_str),
            args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize,
        )
        .unwrap_or_else(error_value)
    }

    fn map(&self, root: &Path, args: &Value) -> Value {
        agent::map(
            self,
            root,
            args.get("prefix").and_then(Value::as_str),
            args.get("depth").and_then(Value::as_u64).unwrap_or(2) as usize,
            args.get("limit").and_then(Value::as_u64).unwrap_or(200) as usize,
        )
        .unwrap_or_else(error_value)
    }

    fn status(&self, root: &Path, _args: &Value) -> Value {
        agent::status(self, root).unwrap_or_else(error_value)
    }

    fn annotate(&self, _root: &Path, args: &Value) -> Value {
        let tags = string_array(args, "tags");
        agent::annotate(
            self,
            args.get("path").and_then(Value::as_str).unwrap_or_default(),
            args.get("summary").and_then(Value::as_str),
            &tags,
            args.get("author")
                .and_then(Value::as_str)
                .unwrap_or("agent"),
        )
        .unwrap_or_else(error_value)
    }
}

impl DocMentions for DocEngine {
    fn chunks_mentioning(&self, names: &[&str]) -> Vec<DocRef> {
        let wanted: HashSet<&str> = names.iter().copied().collect();
        let Ok(mut stmt) = self.conn().prepare(
            "SELECT source_path, section_title, start_line, symbol_references FROM doc_chunks",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
            ))
        }) else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|(path, heading, line, refs)| {
                let refs: Vec<String> = serde_json::from_str(&refs).unwrap_or_default();
                refs.iter()
                    .any(|name| wanted.contains(name.as_str()))
                    .then_some(DocRef {
                        path,
                        heading,
                        line,
                    })
            })
            .collect()
    }
}

/// Multi-workspace adapter used by `serve`. Engines stay open and are selected by root.
pub struct WorkspaceDocsBackend {
    engines: Mutex<Vec<(PathBuf, DocEngine)>>,
}

impl WorkspaceDocsBackend {
    pub fn new(engines: Vec<(PathBuf, DocEngine)>) -> Self {
        Self {
            engines: Mutex::new(engines),
        }
    }

    fn with_engine(&self, root: &Path, f: impl FnOnce(&DocEngine) -> Value) -> Value {
        let engines = self.engines.lock().unwrap_or_else(|e| e.into_inner());
        engines
            .iter()
            .find(|(candidate, _)| candidate == root)
            .map(|(_, engine)| f(engine))
            .unwrap_or_else(|| error_value(format!("workspace not registered: {}", root.display())))
    }
}

impl DocsBackend for WorkspaceDocsBackend {
    fn search(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::search(engine, root, args))
    }
    fn context(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::context(engine, root, args))
    }
    fn inventory(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::inventory(engine, root, args))
    }
    fn map(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::map(engine, root, args))
    }
    fn status(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::status(engine, root, args))
    }
    fn annotate(&self, root: &Path, args: &Value) -> Value {
        self.with_engine(root, |engine| DocsBackend::annotate(engine, root, args))
    }
}
