//! SQLite storage for the code graph: `code_files`, `code_symbols`, `code_calls`,
//! `code_imports`, `code_meta`. Shares the connection used for the docs index (same
//! database file), with `code_`-prefixed tables of its own.
//!
//! See docs/v3/CODEGRAPH.md ("Storage") for the schema and the call-resolution rules.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use super::model::{FileOutline, Lang, SymbolKind};

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS code_files (
    path    TEXT PRIMARY KEY,
    lang    TEXT NOT NULL,
    mtime   INTEGER NOT NULL,
    size    INTEGER NOT NULL,
    sha256  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS code_symbols (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    path         TEXT NOT NULL REFERENCES code_files(path),
    name         TEXT NOT NULL,
    qname        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    start_line   INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    signature    TEXT NOT NULL,
    doc          TEXT NOT NULL,
    parent_qname TEXT
);
CREATE INDEX IF NOT EXISTS idx_code_symbols_path  ON code_symbols(path);
CREATE INDEX IF NOT EXISTS idx_code_symbols_name  ON code_symbols(name);
CREATE INDEX IF NOT EXISTS idx_code_symbols_qname ON code_symbols(qname);

CREATE TABLE IF NOT EXISTS code_calls (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    caller_id INTEGER NOT NULL REFERENCES code_symbols(id),
    callee    TEXT NOT NULL,
    callee_id INTEGER REFERENCES code_symbols(id),
    line      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_code_calls_caller    ON code_calls(caller_id);
CREATE INDEX IF NOT EXISTS idx_code_calls_callee_id ON code_calls(callee_id);

CREATE TABLE IF NOT EXISTS code_imports (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    path   TEXT NOT NULL REFERENCES code_files(path),
    target TEXT NOT NULL,
    names  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_code_imports_path ON code_imports(path);

CREATE TABLE IF NOT EXISTS code_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

const META_EXTRACTOR_VERSION: &str = "extractor_version";

/// Row of `code_files`, as needed by `sync` to decide whether a file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub path: String,
    pub lang: Lang,
    pub mtime: i64,
    pub size: i64,
    pub sha256: String,
}

/// Thin wrapper around a `rusqlite::Connection` holding the code graph tables.
pub struct CodeStore {
    conn: Connection,
}

impl CodeStore {
    /// Opens (or creates) the database file and ensures the schema exists.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(30));
        let _ = conn.query_row("PRAGMA journal_mode = WAL;", [], |_| Ok(()));
        Self::from_connection(conn)
    }

    /// In-memory database: tests, or ephemeral runs.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Wraps an already-open connection (e.g. shared with the docs index) and ensures the
    /// `code_*` schema exists. Idempotent: safe to call again on a database already stamped.
    pub fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA_SQL)
            .context("creating code graph schema")?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Extractor version stamped by the last successful sync, if any.
    pub fn extractor_version(&self) -> Result<Option<u32>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM code_meta WHERE key = ?1",
                params![META_EXTRACTOR_VERSION],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| v.parse::<u32>().context("parsing stored extractor_version"))
            .transpose()
    }

    pub fn set_extractor_version(&self, version: u32) -> Result<()> {
        self.conn.execute(
            "INSERT INTO code_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![META_EXTRACTOR_VERSION, version.to_string()],
        )?;
        Ok(())
    }

    /// Stored metadata for `path`, if it is currently indexed.
    pub fn file_meta(&self, path: &str) -> Result<Option<FileMeta>> {
        self.conn
            .query_row(
                "SELECT path, lang, mtime, size, sha256 FROM code_files WHERE path = ?1",
                params![path],
                |r| {
                    let lang_s: String = r.get(1)?;
                    Ok(FileMeta {
                        path: r.get(0)?,
                        lang: lang_from_str(&lang_s).unwrap_or(Lang::Rust),
                        mtime: r.get(2)?,
                        size: r.get(3)?,
                        sha256: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every path currently indexed (used by `sync` to detect deletions).
    pub fn all_file_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM code_files")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Updates `mtime`/`size` only (content unchanged, confirmed by hash): symbols, calls and
    /// imports stay untouched.
    pub fn touch_file(&self, path: &str, mtime: i64, size: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE code_files SET mtime = ?2, size = ?3 WHERE path = ?1",
            params![path, mtime, size],
        )?;
        Ok(())
    }

    /// Replaces everything known about `path`: symbols, calls and imports are deleted and
    /// re-inserted from `outline`. Transactional.
    pub fn upsert_file(
        &mut self,
        path: &str,
        lang: Lang,
        mtime: i64,
        size: i64,
        sha256: &str,
        outline: &FileOutline,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM code_calls WHERE caller_id IN (SELECT id FROM code_symbols WHERE path = ?1)",
            params![path],
        )?;
        tx.execute("DELETE FROM code_symbols WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM code_imports WHERE path = ?1", params![path])?;
        tx.execute(
            "INSERT INTO code_files (path, lang, mtime, size, sha256) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET lang = excluded.lang, mtime = excluded.mtime,
                size = excluded.size, sha256 = excluded.sha256",
            params![path, lang_to_str(lang), mtime, size, sha256],
        )?;

        // qname -> id, to turn Call.caller_qname into a caller_id within this file.
        let mut qname_to_id: HashMap<&str, i64> = HashMap::new();
        {
            let mut insert_symbol = tx.prepare(
                "INSERT INTO code_symbols
                    (path, name, qname, kind, start_line, end_line, signature, doc, parent_qname)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for sym in &outline.symbols {
                insert_symbol.execute(params![
                    path,
                    sym.name,
                    sym.qname,
                    kind_to_str(sym.kind),
                    sym.start_line,
                    sym.end_line,
                    sym.signature,
                    sym.doc,
                    sym.parent_qname,
                ])?;
                let id = tx.last_insert_rowid();
                qname_to_id.insert(sym.qname.as_str(), id);
            }
        }
        {
            let mut insert_call =
                tx.prepare("INSERT INTO code_calls (caller_id, callee, callee_id, line) VALUES (?1, ?2, NULL, ?3)")?;
            for call in &outline.calls {
                // A call whose caller_qname matches no symbol of this file cannot be stored
                // (code_calls.caller_id is not nullable): dropped silently, documented as a
                // known limitation in docs/v3/CODEGRAPH-NOTES.md.
                if let Some(&caller_id) = qname_to_id.get(call.caller_qname.as_str()) {
                    insert_call.execute(params![caller_id, call.callee, call.line])?;
                }
            }
        }
        {
            let mut insert_import =
                tx.prepare("INSERT INTO code_imports (path, target, names) VALUES (?1, ?2, ?3)")?;
            for imp in &outline.imports {
                let names = serde_json::to_string(&imp.names)?;
                insert_import.execute(params![path, imp.path, names])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes everything known about `path` (symbols, calls, imports, file row). Calls made
    /// *to* symbols of this file are left in place with `callee_id` cleared back to NULL (they
    /// become unresolved again rather than disappearing).
    pub fn remove_file(&mut self, path: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE code_calls SET callee_id = NULL
             WHERE callee_id IN (SELECT id FROM code_symbols WHERE path = ?1)",
            params![path],
        )?;
        tx.execute(
            "DELETE FROM code_calls WHERE caller_id IN (SELECT id FROM code_symbols WHERE path = ?1)",
            params![path],
        )?;
        tx.execute("DELETE FROM code_symbols WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM code_imports WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM code_files WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(())
    }

    /// Recomputes every `code_calls.callee_id` from scratch, following the preference order
    /// documented in CODEGRAPH.md: same file -> files imported by the caller -> same directory
    /// -> unique name repo-wide. Re-running is idempotent and cheap enough for the repo sizes
    /// this tool targets (see docs/v3/CODEGRAPH-NOTES.md for the scaling caveat).
    pub fn resolve_calls(&mut self) -> Result<()> {
        let mut by_path: HashMap<String, Vec<SymRef>> = HashMap::new();
        let mut by_dir: HashMap<String, Vec<SymRef>> = HashMap::new();
        let mut all: Vec<SymRef> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path, name, qname FROM code_symbols")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (id, path, name, qname) = row?;
                let dir = parent_dir(&path);
                let sref = SymRef { id, name, qname };
                by_path.entry(path).or_default().push(sref.clone());
                by_dir.entry(dir).or_default().push(sref.clone());
                all.push(sref);
            }
        }
        let known_paths: Vec<String> = by_path.keys().cloned().collect();

        let mut imports_by_path: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare("SELECT path, target FROM code_imports")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (path, target) = row?;
                imports_by_path.entry(path).or_default().push(target);
            }
        }
        let mut resolved_imports: HashMap<String, Vec<String>> = HashMap::new();
        for (path, targets) in &imports_by_path {
            let mut candidates = Vec::new();
            for t in targets {
                candidates.extend(candidate_paths_for_import(&known_paths, path, t));
            }
            candidates.sort();
            candidates.dedup();
            resolved_imports.insert(path.clone(), candidates);
        }

        struct CallRow {
            id: i64,
            caller_path: String,
            callee: String,
        }
        let mut calls: Vec<CallRow> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT c.id, s.path, c.callee FROM code_calls c JOIN code_symbols s ON s.id = c.caller_id")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (id, caller_path, callee) = row?;
                calls.push(CallRow {
                    id,
                    caller_path,
                    callee,
                });
            }
        }

        let empty: Vec<SymRef> = Vec::new();
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE code_calls SET callee_id = NULL", [])?;
        {
            let mut update = tx.prepare("UPDATE code_calls SET callee_id = ?2 WHERE id = ?1")?;
            for call in &calls {
                let (qualified, terminal) = callee_candidates(&call.callee);
                let dir = parent_dir(&call.caller_path);

                let tier1 = by_path.get(&call.caller_path).unwrap_or(&empty);
                let tier2: Vec<&SymRef> = resolved_imports
                    .get(&call.caller_path)
                    .into_iter()
                    .flatten()
                    .filter_map(|p| by_path.get(p))
                    .flatten()
                    .collect();
                let tier3 = by_dir.get(&dir).unwrap_or(&empty);

                let resolved = match_tier(tier1.iter(), &qualified, &terminal)
                    .or_else(|| match_tier(tier2.iter().copied(), &qualified, &terminal))
                    .or_else(|| match_tier(tier3.iter(), &qualified, &terminal))
                    .or_else(|| match_tier(all.iter(), &qualified, &terminal));

                if let Some(Some(id)) = resolved {
                    update.execute(params![call.id, id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct SymRef {
    id: i64,
    name: String,
    qname: String,
}

/// `Some(Some(id))`: resolved. `Some(None)`: ambiguous within this tier, stop (do not widen).
/// `None`: no match in this tier, try the next (broader) one.
fn match_tier<'a>(
    tier: impl Iterator<Item = &'a SymRef>,
    qualified: &Option<String>,
    terminal: &str,
) -> Option<Option<i64>> {
    let candidates: Vec<&SymRef> = tier.collect();
    if let Some(q) = qualified {
        let matches: Vec<i64> = candidates
            .iter()
            .filter(|s| &s.qname == q)
            .map(|s| s.id)
            .collect();
        if matches.len() == 1 {
            return Some(Some(matches[0]));
        }
        if matches.len() > 1 {
            return Some(None);
        }
    }
    let matches: Vec<i64> = candidates
        .iter()
        .filter(|s| s.name == terminal)
        .map(|s| s.id)
        .collect();
    if matches.len() == 1 {
        return Some(Some(matches[0]));
    }
    if matches.len() > 1 {
        return Some(None);
    }
    None
}

/// `a.b` / `a::b` -> (`Some("a.b")`, `"b"`); bare `foo` -> (`None`, `"foo"`).
fn callee_candidates(callee: &str) -> (Option<String>, String) {
    let normalized = callee.replace("::", ".");
    match normalized.rfind('.') {
        Some(idx) => {
            let terminal = normalized[idx + 1..].to_string();
            (Some(normalized), terminal)
        }
        None => (None, normalized),
    }
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => String::new(),
    }
}

/// Best-effort resolution of an import's textual target to concrete indexed paths: relative
/// (`./foo`, `../bar/baz`) paths are joined with the caller's directory and probed against
/// known source extensions / index files; dotted or slashed module paths (`a.b.C`,
/// `pkg/sub`) are matched as a path suffix. Imprecise by nature (no build-system knowledge);
/// see docs/v3/CODEGRAPH-NOTES.md.
fn candidate_paths_for_import(known: &[String], caller_path: &str, target: &str) -> Vec<String> {
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java", "kt", "kts", "cs",
    ];
    const INDEX_NAMES: &[&str] = &[
        "mod.rs",
        "lib.rs",
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "__init__.py",
    ];
    let known_set: HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let target = target.trim();
    if target.starts_with('.') || target.starts_with('/') {
        let dir = parent_dir(caller_path);
        let joined = normalize_join(&dir, target);
        if known_set.contains(joined.as_str()) {
            out.push(joined.clone());
        }
        for ext in EXTS {
            let candidate = format!("{joined}.{ext}");
            if known_set.contains(candidate.as_str()) {
                out.push(candidate);
            }
        }
        for idx in INDEX_NAMES {
            let candidate = if joined.is_empty() {
                (*idx).to_string()
            } else {
                format!("{joined}/{idx}")
            };
            if known_set.contains(candidate.as_str()) {
                out.push(candidate);
            }
        }
    } else {
        let suffix = if target.contains('/') {
            target.to_string()
        } else if target.contains('.') {
            target.replace('.', "/")
        } else {
            target.to_string()
        };
        let last_segment = suffix.rsplit('/').next().unwrap_or(&suffix).to_string();
        for path in known {
            let no_ext = strip_known_ext(path);
            if no_ext.ends_with(&suffix) || no_ext.ends_with(&format!("/{suffix}")) {
                out.push(path.clone());
                continue;
            }
            if let Some(stem) = Path::new(path).file_stem().and_then(|s| s.to_str()) {
                if stem == last_segment {
                    out.push(path.clone());
                }
            }
        }
    }
    out.dedup();
    out
}

fn strip_known_ext(path: &str) -> &str {
    match path.rfind('.') {
        Some(idx) => &path[..idx],
        None => path,
    }
}

fn normalize_join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(crate) fn lang_to_str(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "rust",
        Lang::TypeScript => "typescript",
        Lang::JavaScript => "javascript",
        Lang::Python => "python",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::Kotlin => "kotlin",
        Lang::CSharp => "csharp",
    }
}

pub(crate) fn lang_from_str(s: &str) -> Option<Lang> {
    Some(match s {
        "rust" => Lang::Rust,
        "typescript" => Lang::TypeScript,
        "javascript" => Lang::JavaScript,
        "python" => Lang::Python,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "kotlin" => Lang::Kotlin,
        "csharp" => Lang::CSharp,
        _ => return None,
    })
}

pub(crate) fn kind_to_str(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Constructor => "constructor",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Trait => "trait",
        SymbolKind::TypeAlias => "type_alias",
        SymbolKind::Const => "const",
        SymbolKind::Variable => "variable",
        SymbolKind::Module => "module",
        SymbolKind::Field => "field",
    }
}

pub(crate) fn kind_from_str(s: &str) -> Option<SymbolKind> {
    Some(match s {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "constructor" => SymbolKind::Constructor,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "interface" => SymbolKind::Interface,
        "trait" => SymbolKind::Trait,
        "type_alias" => SymbolKind::TypeAlias,
        "const" => SymbolKind::Const,
        "variable" => SymbolKind::Variable,
        "module" => SymbolKind::Module,
        "field" => SymbolKind::Field,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::model::{Call, Import, Symbol};

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

    #[test]
    fn schema_creation_is_idempotent() {
        let store = CodeStore::in_memory().unwrap();
        store.conn().execute_batch(SCHEMA_SQL).unwrap();
    }

    #[test]
    fn upsert_then_resolve_same_file_call() {
        let mut store = CodeStore::in_memory().unwrap();
        let outline = FileOutline {
            symbols: vec![sym("main", "main", 1, 5), sym("helper", "helper", 7, 9)],
            calls: vec![call("main", "helper", 2)],
            imports: vec![],
        };
        store
            .upsert_file("a.rs", Lang::Rust, 1, 10, "hash1", &outline)
            .unwrap();
        store.resolve_calls().unwrap();

        let callee_id: Option<i64> = store
            .conn()
            .query_row("SELECT callee_id FROM code_calls", [], |r| r.get(0))
            .unwrap();
        let helper_id: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM code_symbols WHERE qname = 'helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(callee_id, Some(helper_id));
    }

    #[test]
    fn homonyms_in_two_files_prefer_the_imported_one() {
        let mut store = CodeStore::in_memory().unwrap();
        // Two files each define a `helper` function (homonyms), neither in the caller's file
        // or directory. The caller imports `lib/a.rs`: resolution must prefer it over the
        // ambiguous repo-wide match.
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
            imports: vec![Import {
                path: "../lib/a".to_string(),
                names: vec![],
            }],
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

        let a_id: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM code_symbols WHERE path = 'lib/a.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let callee_id: Option<i64> = store
            .conn()
            .query_row(
                "SELECT callee_id FROM code_calls WHERE caller_id = (SELECT id FROM code_symbols WHERE qname = 'main')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(callee_id, Some(a_id));
    }

    #[test]
    fn homonyms_without_import_are_ambiguous() {
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

        let callee_id: Option<i64> = store
            .conn()
            .query_row("SELECT callee_id FROM code_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(callee_id, None);
    }

    #[test]
    fn qualified_callee_prefers_type_qname_over_terminal_name() {
        let mut store = CodeStore::in_memory().unwrap();
        // `Type.new` is unambiguous via the qualified name even though a lone `new` also
        // exists elsewhere in the repo.
        let outline = FileOutline {
            symbols: vec![
                sym("main", "main", 1, 3),
                Symbol {
                    parent_qname: Some("Type".to_string()),
                    ..sym("new", "Type.new", 5, 6)
                },
            ],
            calls: vec![call("main", "Type::new", 2)],
            imports: vec![],
        };
        let other = FileOutline {
            symbols: vec![sym("new", "new", 1, 2)],
            calls: vec![],
            imports: vec![],
        };
        store
            .upsert_file("a.rs", Lang::Rust, 1, 1, "h1", &outline)
            .unwrap();
        store
            .upsert_file("other.rs", Lang::Rust, 1, 1, "h2", &other)
            .unwrap();
        store.resolve_calls().unwrap();

        let type_new_id: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM code_symbols WHERE qname = 'Type.new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let callee_id: Option<i64> = store
            .conn()
            .query_row("SELECT callee_id FROM code_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(callee_id, Some(type_new_id));
    }

    #[test]
    fn remove_file_clears_symbols_and_unresolves_incoming_calls() {
        let mut store = CodeStore::in_memory().unwrap();
        let callee = FileOutline {
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
            .upsert_file("helper.rs", Lang::Rust, 1, 1, "h1", &callee)
            .unwrap();
        store
            .upsert_file("main.rs", Lang::Rust, 1, 1, "h2", &caller)
            .unwrap();
        store.resolve_calls().unwrap();

        store.remove_file("helper.rs").unwrap();

        let files_left: Vec<String> = store.all_file_paths().unwrap();
        assert_eq!(files_left, vec!["main.rs".to_string()]);
        let callee_id: Option<i64> = store
            .conn()
            .query_row("SELECT callee_id FROM code_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(callee_id, None);
    }

    #[test]
    fn extractor_version_round_trips() {
        let store = CodeStore::in_memory().unwrap();
        assert_eq!(store.extractor_version().unwrap(), None);
        store.set_extractor_version(7).unwrap();
        assert_eq!(store.extractor_version().unwrap(), Some(7));
        store.set_extractor_version(8).unwrap();
        assert_eq!(store.extractor_version().unwrap(), Some(8));
    }
}
