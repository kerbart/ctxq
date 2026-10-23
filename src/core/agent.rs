//! Agent-facing API, shared by the MCP server, the daemon and the CLI: one
//! stable JSON shape per operation, bounded in size, so an agent never has to
//! parse prose or load a corpus to answer a factual question.
//!
//! - `search`     — ranked passages (+ metadata, facets)
//! - `inventory`  — structured rows of a generated inventory (no vectors involved)
//! - `map`        — the workspace as a tree of one-line summaries (read the map,
//!   open only what is needed)
//! - `status`     — freshness of the index, sources, stores, quarantines
//! - `annotate`   — what an agent learned about a document, kept for the next one

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::docmeta::SearchFilters;
use crate::core::hybrid::DocEngine;
use crate::core::mdchunk::CRUMB_SEP;

/// Paths are shown relative to the root; as `~/…` when the root is `$HOME`,
/// which is how people and agents refer to them.
pub fn display_path(root: &Path, rel: &str) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let is_home = home
        .and_then(|h| h.canonicalize().ok())
        .zip(root.canonicalize().ok())
        .is_some_and(|(h, r)| h == r);
    if is_home {
        format!("~/{rel}")
    } else {
        rel.to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    #[serde(default = "default_limit", alias = "k")]
    pub limit: usize,
    #[serde(default, flatten)]
    pub filters: SearchFilters,
    /// Characters of text per hit at most (0 = whole chunk).
    #[serde(default)]
    pub max_chars: usize,
}

fn default_limit() -> usize {
    5
}

fn clip(text: &str, max: usize) -> String {
    if max == 0 || text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str(" …");
    out
}

pub fn search(engine: &DocEngine, root: &Path, args: &SearchArgs) -> Result<Value> {
    let hits = engine.search_filtered(&args.query, args.limit.clamp(1, 50), &args.filters)?;
    let results: Vec<Value> = hits
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let source = if r.origin.is_some() {
                r.source.clone()
            } else {
                display_path(root, &r.source)
            };
            json!({
                "rank": i + 1,
                "score": r.score.map(|s| (s * 1000.0).round() / 1000.0),
                "source": source,
                "lines": [r.start_line, r.end_line],
                "title": r.section,
                "breadcrumb": r.hierarchy.as_ref().map(|h| h.join(CRUMB_SEP)),
                "text": clip(&r.text, args.max_chars),
                "meta": r.meta,
                "origin": r.origin,
            })
        })
        .collect();
    Ok(json!({ "query": args.query, "results": results }))
}

/// Rows of the markdown tables of a document, as objects keyed by header.
pub fn parse_tables(md: &str) -> Vec<BTreeMap<String, String>> {
    let lines: Vec<&str> = md.lines().collect();
    let cells = |l: &str| -> Vec<String> {
        l.trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().replace('`', "").replace("<br>", ", "))
            .collect()
    };
    let is_sep = |l: &str| {
        let t = l.trim();
        t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    };
    let mut rows = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        if lines[i].trim_start().starts_with('|') && is_sep(lines[i + 1]) {
            let header = cells(lines[i]);
            let mut j = i + 2;
            while j < lines.len() && lines[j].trim_start().starts_with('|') {
                let row = cells(lines[j]);
                rows.push(
                    header
                        .iter()
                        .cloned()
                        .zip(row.into_iter().chain(std::iter::repeat(String::new())))
                        .collect(),
                );
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    rows
}

/// Structured rows of the inventory produced by source `kind` (its
/// `[[scan.source]] name`), optionally filtered by a case-insensitive
/// substring over every cell. Without `kind`, lists the inventories.
pub fn inventory(
    engine: &DocEngine,
    root: &Path,
    kind: Option<&str>,
    filter: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let settings = engine.settings();
    let dir = root.join(&settings.scan.output_dir);
    let mut names: Vec<String> = settings
        .scan
        .source
        .iter()
        .map(|s| s.name.clone())
        .collect();
    if names.is_empty() {
        // Documents dropped there by other means still count.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            names = rd
                .flatten()
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .collect();
            names.sort();
        }
    }
    let Some(kind) = kind else {
        let list: Vec<Value> = names
            .iter()
            .map(|n| {
                let p = dir.join(format!("{n}.md"));
                let modified = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string());
                json!({ "kind": n, "rows": std::fs::read_to_string(&p).map(|t| parse_tables(&t).len()).unwrap_or(0), "updated": modified })
            })
            .collect();
        return Ok(json!({ "inventories": list }));
    };
    let path = dir.join(format!("{kind}.md"));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| anyhow::anyhow!("no inventory '{kind}' (known: {})", names.join(", ")))?;
    let needle = filter.map(str::to_lowercase);
    let rows: Vec<_> = parse_tables(&text)
        .into_iter()
        .filter(|r| {
            needle
                .as_ref()
                .is_none_or(|n| r.values().any(|v| v.to_lowercase().contains(n)))
        })
        .collect();
    let total = rows.len();
    let rows: Vec<_> = rows.into_iter().take(limit.max(1)).collect();
    Ok(json!({
        "kind": kind,
        "source": display_path(root, &format!("{}/{kind}.md", settings.scan.output_dir)),
        "matched": total,
        "rows": rows,
    }))
}

/// The workspace as a tree: directories → documents with their one-line
/// summary (front matter or annotation, else the title). `prefix` narrows to
/// a sub-tree, `depth` bounds directory nesting, `limit` the documents.
pub fn map(
    engine: &DocEngine,
    root: &Path,
    prefix: Option<&str>,
    depth: usize,
    limit: usize,
) -> Result<Value> {
    let conn = engine.conn();
    let metas = crate::core::docmeta::load_all(conn)?;
    // First chunk of each file = its title / intro.
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT source_path, hierarchy FROM doc_chunks c WHERE rowid = \
             (SELECT MIN(rowid) FROM doc_chunks d WHERE d.source_path = c.source_path)",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            let crumbs: Vec<String> = serde_json::from_str(&row.1).unwrap_or_default();
            titles.insert(row.0, crumbs.first().cloned().unwrap_or_default());
        }
    }
    let prefix = prefix
        .unwrap_or("")
        .trim_start_matches("~/")
        .trim_matches('/');
    let mut tree: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut shown = 0usize;
    let mut skipped = 0usize;
    for (path, title) in &titles {
        if !prefix.is_empty() && !(path == prefix || path.starts_with(&format!("{prefix}/"))) {
            continue;
        }
        let rel = path
            .strip_prefix(prefix)
            .unwrap_or(path)
            .trim_start_matches('/');
        let parts: Vec<&str> = rel.split('/').collect();
        let dir_depth = parts.len().saturating_sub(1);
        let group = if dir_depth > depth {
            // Too deep: fold under its ancestor at `depth`.
            parts[..depth].join("/")
        } else {
            parts[..dir_depth].join("/")
        };
        if shown >= limit {
            skipped += 1;
            continue;
        }
        shown += 1;
        let meta = metas.get(path);
        let summary = meta
            .and_then(|m| m.summary.clone())
            .or_else(|| meta.and_then(|m| m.title.clone()))
            .unwrap_or_else(|| title.clone());
        let mut entry = json!({ "path": display_path(root, path), "summary": clip(&summary, 160) });
        if let Some(m) = meta.filter(|m| !m.tags.is_empty()) {
            entry["tags"] = json!(m.tags);
        }
        tree.entry(if group.is_empty() { ".".into() } else { group })
            .or_default()
            .push(entry);
    }
    Ok(json!({
        "root": display_path(root, prefix).trim_end_matches('/'),
        "documents": shown,
        "omitted": skipped,
        "tree": tree,
    }))
}

pub fn status(engine: &DocEngine, root: &Path) -> Result<Value> {
    let conn = engine.conn();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let meta = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM index_meta WHERE key = ?1", [k], |r| {
            r.get(0)
        })
        .ok()
    };
    let completeness = engine.completeness(root)?;
    let settings = engine.settings();
    let store = match engine.primary_store()? {
        Some(s) => json!({ "backend": s.label(), "points": s.count().ok() }),
        None => json!({ "backend": "local" }),
    };
    let sources: Vec<Value> = settings
        .scan
        .source
        .iter()
        .map(|s| {
            let p = root
                .join(&settings.scan.output_dir)
                .join(format!("{}.md", s.name));
            let age = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs() / 60);
            json!({ "name": s.name, "kind": s.kind, "age_minutes": age })
        })
        .collect();
    Ok(json!({
        "root": root.display().to_string(),
        "config": settings.source_file.as_ref().map(|p| p.display().to_string()),
        "embedder": meta("embedder_id"),
        "chunker": meta("doc_chunker"),
        "last_sync": meta("last_sync"),
        "files": count("SELECT COUNT(*) FROM doc_file_meta"),
        "chunks": count("SELECT COUNT(*) FROM doc_chunks"),
        "store": store,
        "federated": settings.store.federate.iter().map(|f| f.name.clone().unwrap_or_else(|| f.url.clone())).collect::<Vec<_>>(),
        "sources": sources,
        "coverage": {
            "indexed": completeness.indexed,
            "empty": completeness.empty,
            "quarantined": completeness.quarantined.iter().map(|(p, why)| json!({ "path": display_path(root, p), "reason": why })).collect::<Vec<_>>(),
            "unusable": completeness.unusable.len(),
            "unaccounted": completeness.unaccounted.iter().map(|p| display_path(root, p)).collect::<Vec<_>>(),
        },
    }))
}

/// `path` may be given as displayed (`~/x/y.md`) or relative.
pub fn annotate(
    engine: &DocEngine,
    path: &str,
    summary: Option<&str>,
    tags: &[String],
    author: &str,
) -> Result<Value> {
    let rel = path.trim_start_matches("~/");
    crate::core::docmeta::annotate(engine.conn(), rel, summary, tags, author)?;
    engine.invalidate();
    Ok(json!({ "annotated": rel, "summary": summary, "tags": tags }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inventory_tables_into_rows() {
        let md = "# Inv\n\n| Conteneur | Ports |\n|---|---|\n| `myapp` | 127.0.0.1:3917→3917/tcp |\n| `worker` | a<br>b |\n\ntext\n";
        let rows = parse_tables(md);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["Conteneur"], "myapp");
        assert_eq!(rows[1]["Ports"], "a, b");
    }
}
