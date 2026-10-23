//! Read-only queries over the code graph: symbol lookup, call-graph traversal, diff impact,
//! doc cross-references. See docs/v3/CODEGRAPH.md ("Queries").

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Row};
use serde::Serialize;

use super::model::{Lang, SymbolKind};
use super::store::{kind_from_str, lang_from_str, CodeStore};

/// A stored symbol plus its database id and file path (what `find`/`callers`/`callees` hand
/// back; everything here is `Serialize` since it ends up as MCP/CLI JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolHit {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub qname: String,
    pub kind: SymbolKind,
    pub lang: Lang,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
    pub doc: String,
    pub parent_qname: Option<String>,
}

fn hit_from_row(row: &Row) -> rusqlite::Result<SymbolHit> {
    let kind_s: String = row.get("kind")?;
    let lang_s: String = row.get("lang")?;
    Ok(SymbolHit {
        id: row.get("id")?,
        path: row.get("path")?,
        name: row.get("name")?,
        qname: row.get("qname")?,
        kind: kind_from_str(&kind_s).unwrap_or(SymbolKind::Function),
        lang: lang_from_str(&lang_s).unwrap_or(Lang::Rust),
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        signature: row.get("signature")?,
        doc: row.get("doc")?,
        parent_qname: row.get("parent_qname")?,
    })
}

const SELECT_SYMBOL: &str = "SELECT s.id as id, s.path as path, s.name as name, s.qname as qname,
    s.kind as kind, f.lang as lang, s.start_line as start_line, s.end_line as end_line,
    s.signature as signature, s.doc as doc, s.parent_qname as parent_qname
    FROM code_symbols s JOIN code_files f ON f.path = s.path";

/// `name` exact -> qualified name exact/terminal -> prefix -> case-insensitive. Stops at the
/// first non-empty tier.
pub fn find(store: &CodeStore, name: &str) -> Result<Vec<SymbolHit>> {
    let conn = store.conn();

    let exact_name = query_hits(
        conn,
        &format!("{SELECT_SYMBOL} WHERE s.name = ?1"),
        params![name],
    )?;
    if !exact_name.is_empty() {
        return Ok(exact_name);
    }

    let qualified = query_hits(
        conn,
        &format!("{SELECT_SYMBOL} WHERE s.qname = ?1 OR s.qname LIKE ?2 ESCAPE '\\'"),
        params![name, format!("%.{}", escape_like(name))],
    )?;
    if !qualified.is_empty() {
        return Ok(qualified);
    }

    let prefix = query_hits(
        conn,
        &format!("{SELECT_SYMBOL} WHERE s.name LIKE ?1 ESCAPE '\\'"),
        params![escape_like(name) + "%"],
    )?;
    if !prefix.is_empty() {
        return Ok(prefix);
    }

    // Last resort: full Unicode-aware case folding in Rust. SQLite's own `LOWER()`/`LIKE`
    // only fold ASCII, so this is the only tier that actually differs from the prefix tier
    // above for non-ASCII letters (e.g. "Être" vs "être"). Scans every symbol; acceptable
    // since it is only ever reached when the three cheaper tiers found nothing.
    let target = name.to_lowercase();
    let all = query_hits(conn, SELECT_SYMBOL, [])?;
    Ok(all
        .into_iter()
        .filter(|h| h.name.to_lowercase() == target)
        .collect())
}

/// Escapes `%`/`_`/`\` so a user-provided name can be used as a SQL `LIKE` prefix pattern
/// without its own wildcard characters taking effect.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn query_hits(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<SymbolHit>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, hit_from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn hit_by_id(conn: &rusqlite::Connection, id: i64) -> Result<Option<SymbolHit>> {
    let mut hits = query_hits(
        conn,
        &format!("{SELECT_SYMBOL} WHERE s.id = ?1"),
        params![id],
    )?;
    Ok(if hits.is_empty() {
        None
    } else {
        Some(hits.remove(0))
    })
}

/// Rereads `root.join(&hit.path)` and slices `start_line..=end_line` (1-based, inclusive).
pub fn body(root: &Path, hit: &SymbolHit) -> Result<String> {
    let content = fs::read_to_string(root.join(&hit.path))?;
    let start = hit.start_line.max(1) as usize - 1;
    let end = (hit.end_line as usize).min(content.lines().count());
    Ok(content
        .lines()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[derive(Debug, Clone, Serialize)]
pub struct CallEdge {
    pub caller: SymbolHit,
    pub callee: SymbolHit,
    /// The callee text as written at the call site (`"obj.method"`, `"Type::new"`...).
    pub call_text: String,
    pub line: u32,
}

/// A call whose `callee_id` could not be resolved (ambiguous or unknown), met while walking
/// the graph.
#[derive(Debug, Clone, Serialize)]
pub struct AmbiguousCall {
    pub caller: SymbolHit,
    pub callee_text: String,
    pub line: u32,
    /// Symbols sharing the terminal name of `callee_text`, repo-wide, for display purposes.
    pub candidates: Vec<SymbolHit>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CallGraph {
    pub edges: Vec<CallEdge>,
    pub ambiguous: Vec<AmbiguousCall>,
}

fn terminal_name(callee_text: &str) -> &str {
    let normalized = callee_text.trim_end_matches("()");
    normalized
        .rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(normalized)
}

fn candidates_for(conn: &rusqlite::Connection, callee_text: &str) -> Result<Vec<SymbolHit>> {
    query_hits(
        conn,
        &format!("{SELECT_SYMBOL} WHERE s.name = ?1"),
        params![terminal_name(callee_text)],
    )
}

/// Breadth-first, bounded by `depth` hops, never revisiting a symbol. `forward` follows
/// `caller_id -> callee_id` (callees); otherwise the reverse edge (callers).
fn walk(store: &CodeStore, start: i64, depth: u32, forward: bool) -> Result<CallGraph> {
    let conn = store.conn();
    let mut graph = CallGraph::default();
    if depth == 0 {
        return Ok(graph);
    }
    let mut visited: HashSet<i64> = HashSet::from([start]);
    let mut queue: VecDeque<(i64, u32)> = VecDeque::from([(start, 0)]);
    let mut hit_cache: HashMap<i64, SymbolHit> = HashMap::new();

    while let Some((current, hops)) = queue.pop_front() {
        if hops >= depth {
            continue;
        }
        let sql = if forward {
            "SELECT callee, callee_id, line FROM code_calls WHERE caller_id = ?1"
        } else {
            "SELECT caller_id, callee, line FROM code_calls WHERE callee_id = ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows: Vec<(Option<i64>, String, u32)> = if forward {
            stmt.query_map(params![current], |r| {
                Ok((
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(params![current], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (other_id, callee_text, line) in rows {
            let Some(current_hit) = cached_hit(conn, &mut hit_cache, current)? else {
                continue;
            };
            match other_id {
                Some(other_id) => {
                    let Some(other_hit) = cached_hit(conn, &mut hit_cache, other_id)? else {
                        continue;
                    };
                    let (caller, callee) = if forward {
                        (current_hit, other_hit.clone())
                    } else {
                        (other_hit.clone(), current_hit)
                    };
                    graph.edges.push(CallEdge {
                        caller,
                        callee,
                        call_text: callee_text,
                        line,
                    });
                    if visited.insert(other_id) {
                        queue.push_back((other_id, hops + 1));
                    }
                }
                None if forward => {
                    // Unresolved: only meaningful when walking callees (a caller is always
                    // resolved by construction, see store::upsert_file).
                    graph.ambiguous.push(AmbiguousCall {
                        caller: current_hit,
                        candidates: candidates_for(conn, &callee_text)?,
                        callee_text,
                        line,
                    });
                }
                None => {}
            }
        }
    }
    Ok(graph)
}

fn cached_hit(
    conn: &rusqlite::Connection,
    cache: &mut HashMap<i64, SymbolHit>,
    id: i64,
) -> Result<Option<SymbolHit>> {
    if let Some(hit) = cache.get(&id) {
        return Ok(Some(hit.clone()));
    }
    let hit = hit_by_id(conn, id)?;
    if let Some(hit) = &hit {
        cache.insert(id, hit.clone());
    }
    Ok(hit)
}

/// Forward traversal from `id` (functions it calls), bounded by `depth` hops.
pub fn callees(store: &CodeStore, id: i64, depth: u32) -> Result<CallGraph> {
    walk(store, id, depth, true)
}

/// Backward traversal from `id` (functions that call it), bounded by `depth` hops.
pub fn callers(store: &CodeStore, id: i64, depth: u32) -> Result<CallGraph> {
    walk(store, id, depth, false)
}

/// Shortest call path from `a` to `b` (forward edges only), `None` if unreachable within
/// `max_depth` hops.
pub fn path(store: &CodeStore, a: i64, b: i64, max_depth: u32) -> Result<Option<Vec<CallEdge>>> {
    if a == b {
        return Ok(Some(Vec::new()));
    }
    let conn = store.conn();
    let mut visited: HashSet<i64> = HashSet::from([a]);
    let mut queue: VecDeque<(i64, u32)> = VecDeque::from([(a, 0)]);
    // predecessor[id] = (previous id, edge taken to reach id)
    let mut predecessor: HashMap<i64, (i64, CallEdge)> = HashMap::new();
    let mut hit_cache: HashMap<i64, SymbolHit> = HashMap::new();

    while let Some((current, hops)) = queue.pop_front() {
        if hops >= max_depth {
            continue;
        }
        let mut stmt =
            conn.prepare("SELECT callee, callee_id, line FROM code_calls WHERE caller_id = ?1")?;
        let rows: Vec<(Option<i64>, String, u32)> = stmt
            .query_map(params![current], |r| {
                Ok((
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        for (callee_id, callee_text, line) in rows {
            let Some(callee_id) = callee_id else { continue };
            if visited.contains(&callee_id) {
                continue;
            }
            let Some(caller_hit) = cached_hit(conn, &mut hit_cache, current)? else {
                continue;
            };
            let Some(callee_hit) = cached_hit(conn, &mut hit_cache, callee_id)? else {
                continue;
            };
            let edge = CallEdge {
                caller: caller_hit,
                callee: callee_hit,
                call_text: callee_text,
                line,
            };
            visited.insert(callee_id);
            predecessor.insert(callee_id, (current, edge));
            if callee_id == b {
                // Reconstruct the path by walking predecessors back to `a`.
                let mut edges = Vec::new();
                let mut cursor = callee_id;
                while let Some((prev, edge)) = predecessor.remove(&cursor) {
                    edges.push(edge);
                    cursor = prev;
                }
                edges.reverse();
                return Ok(Some(edges));
            }
            queue.push_back((callee_id, hops + 1));
        }
    }
    Ok(None)
}

/// One mentioning location of a symbol in the docs (path, heading, line). Decoupled from the
/// doc store's own chunk representation; whoever owns `search_docs`'s chunk model converts
/// into this.
#[derive(Debug, Clone, Serialize)]
pub struct DocRef {
    pub path: String,
    pub heading: String,
    pub line: u32,
}

/// Bridge to the docs side, implemented and wired in later (kept decoupled from this crate's
/// doc store so `src/code/` has no dependency on it). `names` is typically `[name, qname]`.
pub trait DocMentions {
    fn chunks_mentioning(&self, names: &[&str]) -> Vec<DocRef>;
}

/// Doc passages whose `symbol_references` match `name` or `qname`.
pub fn doc_links(docs: &dyn DocMentions, name: &str, qname: &str) -> Vec<DocRef> {
    let mut refs = docs.chunks_mentioning(&[name, qname]);
    refs.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    refs.dedup_by(|a, b| a.path == b.path && a.line == b.line);
    refs
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImpactReport {
    /// Symbols directly touched by the diff's added/changed line ranges.
    pub touched: Vec<SymbolHit>,
    /// Transitive callers of `touched`, up to `depth` hops, `touched` symbols excluded.
    pub callers: Vec<SymbolHit>,
    pub doc_refs: Vec<DocRef>,
    /// Paths mentioned by the diff that this graph has no record of (not source, not synced,
    /// or deleted).
    pub unresolved_files: Vec<String>,
}

#[derive(Debug, Clone)]
struct Hunk {
    path: String,
    start_line: u32,
    len: u32,
}

/// Parses a `git diff -U0` (or any unified diff): `+++ b/<path>` headers select the current
/// file, `@@ -a,b +c,d @@` hunks give the new-file line range. Pure-deletion hunks (`d == 0`)
/// are anchored on `c` (best-effort: the surrounding line, since nothing was actually added).
fn parse_diff_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current_path: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let p = rest.trim();
            current_path = if p == "/dev/null" {
                None
            } else {
                Some(
                    p.strip_prefix("b/")
                        .or_else(|| p.strip_prefix("a/"))
                        .unwrap_or(p)
                        .to_string(),
                )
            };
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let Some(path) = &current_path else { continue };
            // rest looks like "-a,b +c,d @@ optional context"
            let Some(plus_idx) = rest.find('+') else {
                continue;
            };
            let after_plus = &rest[plus_idx + 1..];
            let spec = after_plus.split(" @@").next().unwrap_or(after_plus).trim();
            let (start_s, len_s) = spec.split_once(',').unwrap_or((spec, "1"));
            let Ok(start) = start_s.trim().parse::<u32>() else {
                continue;
            };
            let len = len_s.trim().parse::<u32>().unwrap_or(1);
            let (anchor, anchor_len) = if len == 0 {
                (start.max(1), 1)
            } else {
                (start, len)
            };
            hunks.push(Hunk {
                path: path.clone(),
                start_line: anchor,
                len: anchor_len,
            });
        }
    }
    hunks
}

/// Symbols touched by a `git diff -U0`, then their transitive callers up to `depth` hops, plus
/// doc mentions when `docs` is supplied (kept optional: the docs-side implementation lands
/// separately).
pub fn impact(
    store: &CodeStore,
    diff_text: &str,
    depth: u32,
    docs: Option<&dyn DocMentions>,
) -> Result<ImpactReport> {
    let conn = store.conn();
    let hunks = parse_diff_hunks(diff_text);

    let mut touched: Vec<SymbolHit> = Vec::new();
    let mut touched_ids: HashSet<i64> = HashSet::new();
    let mut unresolved_files: Vec<String> = Vec::new();

    for hunk in &hunks {
        let known: bool = conn
            .query_row(
                "SELECT 1 FROM code_files WHERE path = ?1",
                params![hunk.path],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !known {
            if !unresolved_files.contains(&hunk.path) {
                unresolved_files.push(hunk.path.clone());
            }
            continue;
        }
        let range_end = hunk.start_line + hunk.len.saturating_sub(1);
        let hits = query_hits(
            conn,
            &format!(
                "{SELECT_SYMBOL} WHERE s.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?3"
            ),
            params![hunk.path, range_end, hunk.start_line],
        )?;
        for hit in hits {
            if touched_ids.insert(hit.id) {
                touched.push(hit);
            }
        }
    }

    let mut caller_ids: HashSet<i64> = HashSet::new();
    let mut callers_out: Vec<SymbolHit> = Vec::new();
    for hit in &touched {
        let graph = callers(store, hit.id, depth)?;
        for edge in graph.edges {
            if !touched_ids.contains(&edge.caller.id) && caller_ids.insert(edge.caller.id) {
                callers_out.push(edge.caller);
            }
        }
    }

    let doc_refs = if let Some(docs) = docs {
        let names: Vec<&str> = touched
            .iter()
            .flat_map(|h| [h.name.as_str(), h.qname.as_str()])
            .collect();
        let mut refs = docs.chunks_mentioning(&names);
        refs.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        refs.dedup_by(|a, b| a.path == b.path && a.line == b.line);
        refs
    } else {
        Vec::new()
    };

    Ok(ImpactReport {
        touched,
        callers: callers_out,
        doc_refs,
        unresolved_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::model::{Call, FileOutline, Symbol};
    use crate::code::store::CodeStore;

    fn sym(name: &str, qname: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            name: name.to_string(),
            qname: qname.to_string(),
            kind: SymbolKind::Function,
            lang: Lang::Rust,
            start_line: start,
            end_line: end,
            signature: format!("fn {name}()"),
            doc: String::new(),
            parent_qname: None,
        }
    }

    fn call(caller_qname: &str, callee: &str, line: u32) -> Call {
        Call {
            caller_qname: caller_qname.to_string(),
            callee: callee.to_string(),
            line,
        }
    }

    fn id_of(store: &CodeStore, qname: &str) -> i64 {
        store
            .conn()
            .query_row(
                "SELECT id FROM code_symbols WHERE qname = ?1",
                params![qname],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn find_tries_name_then_qname_then_prefix_then_unicode_case_fold() {
        let mut store = CodeStore::in_memory().unwrap();
        let outline = FileOutline {
            symbols: vec![
                sym("run", "Helper.run", 1, 2),
                sym("runner", "runner", 3, 4),
                sym("Être", "Être", 5, 6),
            ],
            calls: vec![],
            imports: vec![],
        };
        store
            .upsert_file("a.rs", Lang::Rust, 1, 1, "h", &outline)
            .unwrap();

        // Tier 1: exact name.
        let hits = find(&store, "run").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].qname, "Helper.run");

        // Tier 2: searching by the qualified form reaches it via qname, not name.
        let hits = find(&store, "Helper.run").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "run");

        // Tier 3: prefix ("runn" matches no name/qname exactly, but is a prefix of "runner").
        let hits = find(&store, "runn").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "runner");

        // Tier 4: Unicode-aware case folding ("Ê"/"ê" is outside SQLite's ASCII-only LIKE/LOWER).
        let hits = find(&store, "être").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Être");

        assert!(find(&store, "does-not-exist").unwrap().is_empty());
    }

    #[test]
    fn body_reads_declared_line_range() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn a() {}\nfn b() {\n    1\n}\nfn c() {}\n",
        )
        .unwrap();
        let mut store = CodeStore::in_memory().unwrap();
        let outline = FileOutline {
            symbols: vec![sym("b", "b", 2, 4)],
            calls: vec![],
            imports: vec![],
        };
        store
            .upsert_file("a.rs", Lang::Rust, 1, 1, "h", &outline)
            .unwrap();
        let hit = find(&store, "b").unwrap().remove(0);
        let text = body(dir.path(), &hit).unwrap();
        assert_eq!(text, "fn b() {\n    1\n}");
    }

    #[test]
    fn callees_and_callers_follow_a_cycle_without_looping() {
        let mut store = CodeStore::in_memory().unwrap();
        // a -> b -> c -> a (cycle)
        let outline = FileOutline {
            symbols: vec![
                sym("a", "a", 1, 2),
                sym("b", "b", 3, 4),
                sym("c", "c", 5, 6),
            ],
            calls: vec![call("a", "b", 1), call("b", "c", 3), call("c", "a", 5)],
            imports: vec![],
        };
        store
            .upsert_file("x.rs", Lang::Rust, 1, 1, "h", &outline)
            .unwrap();
        store.resolve_calls().unwrap();

        let a_id = id_of(&store, "a");
        let graph = callees(&store, a_id, 10).unwrap();
        // Must terminate (bounded by visited-set) and contain exactly the 3 edges of the cycle.
        assert_eq!(graph.edges.len(), 3);

        let graph_depth1 = callees(&store, a_id, 1).unwrap();
        assert_eq!(graph_depth1.edges.len(), 1);
        assert_eq!(graph_depth1.edges[0].callee.qname, "b");

        let callers_of_a = callers(&store, a_id, 10).unwrap();
        assert_eq!(callers_of_a.edges.len(), 3);
    }

    #[test]
    fn path_finds_shortest_route_within_max_depth() {
        let mut store = CodeStore::in_memory().unwrap();
        let outline = FileOutline {
            symbols: vec![
                sym("a", "a", 1, 2),
                sym("b", "b", 3, 4),
                sym("c", "c", 5, 6),
                sym("d", "d", 7, 8),
            ],
            calls: vec![
                call("a", "b", 1),
                call("b", "c", 3),
                call("a", "d", 1),
                call("d", "c", 7),
            ],
            imports: vec![],
        };
        store
            .upsert_file("x.rs", Lang::Rust, 1, 1, "h", &outline)
            .unwrap();
        store.resolve_calls().unwrap();

        let a_id = id_of(&store, "a");
        let c_id = id_of(&store, "c");
        let found = path(&store, a_id, c_id, 5).unwrap().unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].caller.qname, "a");
        assert_eq!(found.last().unwrap().callee.qname, "c");

        assert!(path(&store, a_id, c_id, 0).unwrap().is_none());
    }

    #[test]
    fn ambiguous_calls_are_reported_not_silently_dropped() {
        let mut store = CodeStore::in_memory().unwrap();
        let a = FileOutline {
            symbols: vec![sym("helper", "helper", 1, 2)],
            calls: vec![],
            imports: vec![],
        };
        let b = FileOutline {
            symbols: vec![sym("helper", "helper", 1, 2)],
            calls: vec![],
            imports: vec![],
        };
        let caller = FileOutline {
            symbols: vec![sym("main", "main", 1, 3)],
            calls: vec![call("main", "helper", 2)],
            imports: vec![],
        };
        store
            .upsert_file("lib/a.rs", Lang::Rust, 1, 1, "h1", &a)
            .unwrap();
        store
            .upsert_file("lib/b.rs", Lang::Rust, 1, 1, "h2", &b)
            .unwrap();
        store
            .upsert_file("cmd/main.rs", Lang::Rust, 1, 1, "h3", &caller)
            .unwrap();
        store.resolve_calls().unwrap();

        let main_id = id_of(&store, "main");
        let graph = callees(&store, main_id, 1).unwrap();
        assert!(graph.edges.is_empty());
        assert_eq!(graph.ambiguous.len(), 1);
        assert_eq!(graph.ambiguous[0].candidates.len(), 2);
    }

    #[test]
    fn impact_covers_a_multi_file_diff() {
        let mut store = CodeStore::in_memory().unwrap();
        let lib = FileOutline {
            symbols: vec![sym("helper", "helper", 1, 5)],
            calls: vec![],
            imports: vec![],
        };
        let app = FileOutline {
            symbols: vec![sym("main", "main", 1, 3), sym("other", "other", 5, 7)],
            calls: vec![call("main", "helper", 2)],
            imports: vec![],
        };
        store
            .upsert_file("lib.rs", Lang::Rust, 1, 1, "h1", &lib)
            .unwrap();
        store
            .upsert_file("app.rs", Lang::Rust, 1, 1, "h2", &app)
            .unwrap();
        store.resolve_calls().unwrap();

        let diff = "\
diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -2,0 +3,1 @@
+// touched inside helper
diff --git a/app.rs b/app.rs
--- a/app.rs
+++ b/app.rs
@@ -6,0 +7,1 @@
+// touched inside other
diff --git a/unknown.rs b/unknown.rs
--- a/unknown.rs
+++ b/unknown.rs
@@ -1,0 +2,1 @@
+// a file never synced
";
        let report = impact(&store, diff, 5, None).unwrap();
        let touched_qnames: HashSet<&str> =
            report.touched.iter().map(|h| h.qname.as_str()).collect();
        assert_eq!(touched_qnames, HashSet::from(["helper", "other"]));
        assert_eq!(report.unresolved_files, vec!["unknown.rs".to_string()]);

        let caller_qnames: HashSet<&str> =
            report.callers.iter().map(|h| h.qname.as_str()).collect();
        assert_eq!(caller_qnames, HashSet::from(["main"]));
    }
}
