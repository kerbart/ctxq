//! Document engine: SQLite index, incremental sync, dense + BM25 fusion.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::core::collect::DiscoveryFilter;
use crate::core::convert::{self, is_convertible};
use crate::core::docmeta::{self, DocMeta, SearchFilters};
use crate::core::doctext;
use crate::core::lexical::{self, IdRouter};
use crate::core::mdchunk::{self, CRUMB_SEP};
use crate::core::parse_issues::{self, ParseIssue, KIND_QUARANTINED, KIND_UNUSABLE};
use crate::core::parser::{parse_markdown, DocChunk};
use crate::core::ranking::{self, FTS_CHANNEL_WEIGHT, RRF_K};
use crate::core::secrets::SecretScanner;
use crate::core::settings::{Settings, StoreMode};
use crate::core::store::{self, StorePoint, VectorStore};
use crate::core::vector::Embedder;

pub const CHUNKER_VERSION: &str = "md-h2h3-v1";
const SCHEMA_VERSION: &str = "1";

/// Live progress for `index` / `sync`. All methods have empty defaults so
/// callers that do not care (daemon, tests) pass `None`.
pub trait IndexHooks {
    /// One more file found while walking the tree (total still unknown).
    fn on_scan(&mut self, found: usize, rel: &str) {
        let _ = (found, rel);
    }
    /// Walk finished; `total` files will be indexed.
    fn on_plan(&mut self, total: usize) {
        let _ = total;
    }
    /// One file of the planned set was processed (`i` is 0-based).
    fn on_file(&mut self, i: usize, total: usize, rel: &str, changed: bool) {
        let _ = (i, total, rel, changed);
    }
    /// One source file visited while refreshing the code graph.
    fn on_code(&mut self, scanned: usize, rel: &str) {
        let _ = (scanned, rel);
    }
}

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub files: usize,
    pub chunks: usize,
    pub changed: usize,
    pub empty: usize,
    pub quarantined: usize,
    pub unusable: usize,
}

#[derive(Debug, Clone)]
pub struct Completeness {
    pub indexed: usize,
    pub empty: usize,
    pub quarantined: Vec<(String, String)>,
    pub unusable: Vec<(String, String)>,
    pub unaccounted: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub source: String,
    pub section: String,
    pub hierarchy: Option<Vec<String>>,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: Option<f32>,
    pub meta: Option<DocMeta>,
    pub origin: Option<String>,
}

/// `cos + α · bm25 / bm25_max`. A zero `bm25_max` gives no lexical contribution.
pub fn fuse_score(cosine: f32, bm25: f32, bm25_max: f32, alpha: f32) -> f32 {
    let norm = if bm25_max > 1e-6 {
        bm25 / bm25_max
    } else {
        0.0
    };
    cosine + alpha * norm
}

struct MemIndex {
    chunks: Vec<DocChunk>,
    vectors: Vec<Vec<f32>>,
    corpus: lexical::Corpus,
    meta: HashMap<String, DocMeta>,
    modified: HashMap<String, String>,
}

pub struct DocEngine {
    conn: Connection,
    embedder: Embedder,
    settings: Settings,
    root: Option<PathBuf>,
    cache: Mutex<Option<MemIndex>>,
}

impl DocEngine {
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        init_schema(&conn)?;
        Ok(Self::from_conn(
            conn,
            Embedder::local(),
            Settings::default(),
            None,
        ))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = open_conn(path.as_ref())?;
        let (settings, root) = settings_from_conn(&conn);
        let embedder = Embedder::from_settings(&settings, None)?;
        Ok(Self::from_conn(conn, embedder, settings, root))
    }

    pub fn open_with_embedder(path: &Path, embedder: Embedder) -> Result<Self> {
        let conn = open_conn(path)?;
        let (settings, root) = settings_from_conn(&conn);
        Ok(Self::from_conn(conn, embedder, settings, root))
    }

    fn from_conn(
        conn: Connection,
        embedder: Embedder,
        settings: Settings,
        root: Option<PathBuf>,
    ) -> Self {
        Self {
            conn,
            embedder,
            settings,
            root,
            cache: Mutex::new(None),
        }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn settings(&self) -> Settings {
        if let Some(root) = &self.root {
            if let Ok(s) = Settings::load(root) {
                return s;
            }
        }
        self.settings.clone()
    }

    pub fn invalidate(&self) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn primary_store(&self) -> Result<Option<Box<dyn VectorStore>>> {
        store::open_primary(&self.settings().store)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, limit, &SearchFilters::default())
    }

    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.ensure_vectors_coherent()?;
        let settings = self.settings();
        self.memory()?;
        let qv = self.embed_query_cached(query)?;
        let guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mem = guard.as_ref().context("in-memory index")?;
        if mem.chunks.is_empty() {
            return Ok(Vec::new());
        }
        let allowed = allowed_sources(mem, filters);
        let router = IdRouter::new(settings.search.lookup_terms.as_deref())?;
        let mut alpha = if router.is_identifier_query(query) {
            settings.search.alpha_id
        } else {
            settings.search.alpha
        };
        // local-hash cosine is a weak signal (MRR ≈ 0.51 alone). Let BM25
        // dominate: score = cosine + α·bm25_norm, so α ≥ 1.5 means lexical
        // can outweigh a high hash cosine.
        if self.embedder.is_local_hash() {
            alpha = alpha.max(1.5);
        }
        let qtoks: HashSet<String> = lexical::tokenize(query).into_iter().collect();
        let hits = match settings.search.fusion {
            crate::core::settings::Fusion::Rrf => rrf_hits(mem, &qv, &qtoks, &allowed, limit),
            crate::core::settings::Fusion::Score => score_hits(
                mem,
                ScoreOpts {
                    qv: &qv,
                    qtoks: &qtoks,
                    allowed: &allowed,
                    limit,
                    alpha,
                    settings: &settings,
                    filtered: !filters.is_empty(),
                },
            ),
        };
        let mut hits = attach_meta(hits, mem);
        drop(guard);
        self.federate(query, &qv, limit, &settings, &mut hits)?;
        if settings.search.max_per_source > 0 {
            hits = cap_per_source(hits, settings.search.max_per_source);
        }
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn completeness(&self, root: &Path) -> Result<Completeness> {
        let settings = self.settings();
        let files = discover(root, &settings)?;
        let mut indexed = 0;
        let mut empty = 0;
        let mut quarantined = Vec::new();
        let mut unusable = Vec::new();
        let mut unaccounted = Vec::new();
        for rel in files {
            let issues = issues_for(&self.conn, &rel)?;
            if let Some(reason) = issues
                .iter()
                .find(|i| i.kind == KIND_QUARANTINED)
                .map(|i| i.reason.clone())
            {
                quarantined.push((rel, reason));
                continue;
            }
            if let Some(reason) = issues
                .iter()
                .find(|i| i.kind == KIND_UNUSABLE)
                .map(|i| i.reason.clone())
            {
                unusable.push((rel, reason));
                continue;
            }
            let chunks: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM doc_chunks WHERE source_path = ?1",
                    [&rel],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if chunks > 0 {
                indexed += 1;
                continue;
            }
            let known: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM doc_file_meta WHERE source_path = ?1)",
                    [&rel],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if known {
                empty += 1;
            } else {
                unaccounted.push(rel);
            }
        }
        Ok(Completeness {
            indexed,
            empty,
            quarantined,
            unusable,
            unaccounted,
        })
    }

    pub fn sync_directory_incremental(
        &mut self,
        root: &Path,
        hooks: Option<&mut dyn IndexHooks>,
    ) -> Result<SyncReport> {
        self.sync_root(root, false, hooks)
    }

    pub fn reindex_directory(
        &mut self,
        root: &Path,
        hooks: Option<&mut dyn IndexHooks>,
    ) -> Result<SyncReport> {
        self.sync_root(root, true, hooks)
    }

    fn sync_root(
        &mut self,
        root: &Path,
        force: bool,
        mut hooks: Option<&mut dyn IndexHooks>,
    ) -> Result<SyncReport> {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.root = Some(root.clone());
        self.settings = Settings::load(&root).unwrap_or_else(|_| Settings::default());
        let scanner = SecretScanner::new(&self.settings.secrets)?;
        let files = if let Some(ref mut h) = hooks {
            discover_with(&root, &self.settings, Some(&mut **h))?
        } else {
            discover(&root, &self.settings)?
        };
        if let Some(h) = hooks.as_mut() {
            h.on_plan(files.len());
        }
        let stored_chunker = meta_get(&self.conn, "doc_chunker");
        let stored_embedder = meta_get(&self.conn, "embedder_id");
        let has_chunks: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))
            .unwrap_or(0);
        let embedder_id = self.embedder.id();
        let full = force
            || (has_chunks > 0 && stored_chunker.as_deref() != Some(CHUNKER_VERSION))
            || (has_chunks > 0 && stored_embedder.as_deref() != Some(embedder_id.as_str()));
        let per_row_dirs = self.settings.table_row_dirs();
        let roots = self.settings.collect.roots.clone();
        let dedup_min = self.settings.search.dedup_min_chars;
        let mut seen: HashSet<String> = HashSet::new();
        let mut metas: Vec<(String, DocMeta)> = Vec::new();
        let mut changed_paths: Vec<String> = Vec::new();
        let mut n_changed = 0;
        let mut n_empty = 0;
        let mut n_quar = 0;
        let mut n_unusable = 0;
        let total = files.len();
        for (i, rel) in files.iter().enumerate() {
            seen.insert(rel.clone());
            let path = root.join(rel);
            let meta_ok = !full && file_unchanged(&self.conn, rel, &path).unwrap_or(false);
            if meta_ok {
                if let Some(h) = hooks.as_mut() {
                    h.on_file(i, total, rel, false);
                }
                let text = read_for_meta(&path);
                metas.push((
                    rel.clone(),
                    docmeta::from_content(rel, text.as_deref().unwrap_or("")),
                ));
                continue;
            }
            let outcome = ingest_file(&path, rel, &scanner, &per_row_dirs);
            delete_source(&self.conn, rel)?;
            match outcome {
                Outcome::Quarantine(reason) => {
                    n_quar += 1;
                    record_file(&self.conn, rel, &path)?;
                    parse_issues::set_for_file(
                        &self.conn,
                        rel,
                        &[ParseIssue {
                            file_path: rel.clone(),
                            kind: KIND_QUARANTINED.to_string(),
                            reason,
                            severity: "warning".into(),
                        }],
                    )?;
                    metas.push((rel.clone(), docmeta::from_content(rel, "")));
                }
                Outcome::Unusable(reason) => {
                    n_unusable += 1;
                    record_file(&self.conn, rel, &path)?;
                    parse_issues::set_for_file(
                        &self.conn,
                        rel,
                        &[ParseIssue {
                            file_path: rel.clone(),
                            kind: KIND_UNUSABLE.to_string(),
                            reason,
                            severity: "warning".into(),
                        }],
                    )?;
                    metas.push((rel.clone(), docmeta::from_content(rel, "")));
                }
                Outcome::Empty => {
                    n_empty += 1;
                    record_file(&self.conn, rel, &path)?;
                    parse_issues::set_for_file(&self.conn, rel, &[])?;
                    metas.push((rel.clone(), docmeta::from_content(rel, "")));
                }
                Outcome::Chunks(text, chunks) => {
                    record_file(&self.conn, rel, &path)?;
                    parse_issues::set_for_file(&self.conn, rel, &[])?;
                    store_chunks(&self.conn, &chunks)?;
                    self.embed_chunks(&chunks)?;
                    metas.push((rel.clone(), docmeta::from_content(rel, &text)));
                }
            }
            n_changed += 1;
            changed_paths.push(rel.clone());
            if let Some(h) = hooks.as_mut() {
                h.on_file(i, total, rel, true);
            }
        }
        let removed = paths_not_in(&self.conn, "doc_file_meta", &seen)?;
        for rel in &removed {
            delete_source(&self.conn, rel)?;
            self.conn
                .execute("DELETE FROM doc_file_meta WHERE source_path = ?1", [rel])?;
        }
        parse_issues::retain(&self.conn, &seen)?;
        let dropped = dedup_chunks(&self.conn, &roots, dedup_min)?;
        for rel in dropped {
            if !changed_paths.contains(&rel) {
                changed_paths.push(rel);
            }
        }
        let meta_refs: Vec<(&str, &DocMeta)> = metas.iter().map(|(p, m)| (p.as_str(), m)).collect();
        docmeta::upsert_many(&mut self.conn, &meta_refs)?;
        docmeta::retain(&mut self.conn, &seen)?;
        self.push_store(&changed_paths, &removed)?;
        let chunks: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))?;
        meta_set(&self.conn, "schema_version", SCHEMA_VERSION)?;
        meta_set(&self.conn, "doc_chunker", CHUNKER_VERSION)?;
        meta_set(&self.conn, "embedder_id", &embedder_id)?;
        meta_set(&self.conn, "embedder_dim", &self.embedder.dim().to_string())?;
        meta_set(&self.conn, "workspace_root", &root.to_string_lossy())?;
        meta_set(&self.conn, "last_sync", &now_iso())?;
        self.invalidate();
        Ok(SyncReport {
            files: files.len(),
            chunks: chunks as usize,
            changed: n_changed,
            empty: n_empty,
            quarantined: n_quar,
            unusable: n_unusable,
        })
    }

    fn embed_chunks(&self, chunks: &[DocChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let inputs: Vec<String> = chunks.iter().map(embed_input).collect();
        let vectors = self.embed_documents_cached(&inputs)?;
        for (ch, vec) in chunks.iter().zip(vectors) {
            self.conn.execute(
                "INSERT OR REPLACE INTO chunk_vectors (chunk_id, vector) VALUES (?1, ?2)",
                params![ch.id, vec_to_blob(&vec)],
            )?;
        }
        Ok(())
    }

    fn embed_documents_cached(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let id = self.embedder.id();
        let mut out = vec![Vec::new(); texts.len()];
        let mut missing_idx = Vec::new();
        let mut missing_txt = Vec::new();
        for (i, text) in texts.iter().enumerate() {
            let dig = digest(text);
            if let Some(v) = cache_get(&self.conn, &id, &dig)? {
                out[i] = v;
            } else {
                missing_idx.push(i);
                missing_txt.push(text.clone());
            }
        }
        if !missing_txt.is_empty() {
            let fresh = self.embedder.embed_documents(&missing_txt)?;
            if fresh.len() != missing_txt.len() {
                bail!(
                    "embedder: {} vectors for {} texts",
                    fresh.len(),
                    missing_txt.len()
                );
            }
            for (i, vec) in missing_idx.into_iter().zip(fresh) {
                cache_put(&self.conn, &id, &digest(&texts[i]), &vec)?;
                out[i] = vec;
            }
        }
        Ok(out)
    }

    fn embed_query_cached(&self, query: &str) -> Result<Vec<f32>> {
        let id = format!("query:{}", self.embedder.id());
        let dig = digest(query);
        if let Some(v) = cache_get(&self.conn, &id, &dig)? {
            return Ok(v);
        }
        let v = self.embedder.embed_query(query)?;
        cache_put(&self.conn, &id, &dig, &v)?;
        Ok(v)
    }

    fn ensure_vectors_coherent(&self) -> Result<()> {
        let stored = meta_get(&self.conn, "embedder_id");
        let has: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))
            .unwrap_or(0);
        if has > 0 {
            if let Some(id) = stored {
                if id != self.embedder.id() {
                    bail!(
                        "inconsistent vectors (index {id}, embedder {}). Re-run `ctxq sync` to re-embed.",
                        self.embedder.id()
                    );
                }
            }
        }
        Ok(())
    }

    fn memory(&self) -> Result<()> {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return Ok(());
        }
        let mut chunks = Vec::new();
        let mut vectors = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT c.id, c.source_path, c.section_title, c.heading_level, c.content, c.start_line, c.end_line, c.hierarchy, v.vector
                 FROM doc_chunks c LEFT JOIN chunk_vectors v ON v.chunk_id = c.id
                 ORDER BY c.source_path, c.start_line, c.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as u32,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)? as usize,
                    r.get::<_, i64>(6)? as usize,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<Vec<u8>>>(8)?,
                ))
            })?;
            for row in rows {
                let (id, source, title, level, content, start, end, hierarchy, blob) = row?;
                let hierarchy: Vec<String> = serde_json::from_str(&hierarchy).unwrap_or_default();
                chunks.push(DocChunk {
                    id,
                    source_path: source,
                    section_title: title,
                    heading_level: level,
                    content,
                    start_line: start,
                    end_line: end,
                    hierarchy,
                    symbol_references: Vec::new(),
                });
                vectors.push(blob.map(|b| blob_to_vec(&b)).unwrap_or_default());
            }
        }
        let corpus = lexical::Corpus::new(&chunks);
        let meta = docmeta::load_all(&self.conn).unwrap_or_default();
        let mut modified = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT source_path, mtime_millis FROM doc_file_meta")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows.flatten() {
                modified.insert(row.0, ymd_from_millis(row.1));
            }
        }
        *guard = Some(MemIndex {
            chunks,
            vectors,
            corpus,
            meta,
            modified,
        });
        Ok(())
    }

    fn federate(
        &self,
        query: &str,
        qv: &[f32],
        limit: usize,
        settings: &Settings,
        hits: &mut Vec<SearchHit>,
    ) -> Result<()> {
        let federated = match store::open_federated(&settings.store) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("federation skipped: {e:#}");
                return Ok(());
            }
        };
        if federated.is_empty() {
            return Ok(());
        }
        let k = limit.max(1);
        let mut extra = Vec::new();
        std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for (cfg, store) in &federated {
                let q = query;
                let v = qv;
                joins.push(scope.spawn(move || {
                    let label = cfg.name.clone().unwrap_or_else(|| cfg.url.clone());
                    match store.query(q, v, k) {
                        Ok(found) => Ok((label, cfg.weight, found)),
                        Err(e) => {
                            tracing::warn!("federated store {label} unreachable: {e:#}");
                            Err(())
                        }
                    }
                }));
            }
            for join in joins {
                if let Ok(Ok((label, weight, found))) = join.join() {
                    for h in found {
                        let Some(payload) = h.point else { continue };
                        let start = payload.start_line.unwrap_or(0);
                        let end = payload.end_line.unwrap_or(start);
                        let hierarchy: Vec<String> = if payload.title.is_empty() {
                            Vec::new()
                        } else {
                            payload
                                .title
                                .split(CRUMB_SEP)
                                .map(|s| s.to_string())
                                .collect()
                        };
                        extra.push(SearchHit {
                            source: payload.source,
                            section: hierarchy.last().cloned().unwrap_or_default(),
                            hierarchy: Some(hierarchy).filter(|h| !h.is_empty()),
                            text: payload.text,
                            start_line: start,
                            end_line: end,
                            score: Some(h.score * weight),
                            meta: None,
                            origin: Some(label.clone()),
                        });
                    }
                }
            }
        });
        let local_keys: HashSet<(String, usize)> = hits
            .iter()
            .map(|h| (h.source.clone(), h.start_line))
            .collect();
        for h in extra {
            if local_keys.contains(&(h.source.clone(), h.start_line)) {
                continue;
            }
            hits.push(h);
        }
        sort_hits(hits);
        Ok(())
    }

    fn push_store(&self, changed: &[String], removed: &[String]) -> Result<()> {
        let settings = self.settings.clone();
        if settings.store.mode != StoreMode::Owned {
            return Ok(());
        }
        let Some(store) = store::open_primary(&settings.store)? else {
            return Ok(());
        };
        if !removed.is_empty() {
            store.delete_sources(removed)?;
        }
        if changed.is_empty() {
            return Ok(());
        }
        let mut points = Vec::new();
        for rel in changed {
            let mut stmt = self.conn.prepare(
                "SELECT c.id, c.source_path, c.content, c.start_line, c.end_line, c.hierarchy, v.vector
                 FROM doc_chunks c JOIN chunk_vectors v ON v.chunk_id = c.id
                 WHERE c.source_path = ?1",
            )?;
            let rows = stmt.query_map([rel], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as usize,
                    r.get::<_, i64>(4)? as usize,
                    r.get::<_, String>(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                ))
            })?;
            for row in rows {
                let (id, source, text, start, end, hierarchy, blob) = row?;
                let crumbs: Vec<String> = serde_json::from_str(&hierarchy).unwrap_or_default();
                points.push(StorePoint {
                    chunk_id: id,
                    vector: blob_to_vec(&blob),
                    source,
                    breadcrumb: crumbs.join(CRUMB_SEP),
                    text,
                    start_line: start,
                    end_line: end,
                });
            }
        }
        if points.is_empty() {
            return Ok(());
        }
        let dim = points[0].vector.len();
        store.ensure(dim)?;
        for batch in points.chunks(128) {
            store.upsert(batch)?;
        }
        Ok(())
    }
}

fn sort_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        let sa = a.score.unwrap_or(f32::NEG_INFINITY);
        let sb = b.score.unwrap_or(f32::NEG_INFINITY);
        sb.total_cmp(&sa)
            .then(a.source.cmp(&b.source))
            .then(a.start_line.cmp(&b.start_line))
            .then(a.origin.cmp(&b.origin))
    });
}

fn cap_per_source(hits: Vec<SearchHit>, max_per: usize) -> Vec<SearchHit> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for h in hits {
        let n = counts.entry(h.source.clone()).or_insert(0);
        if *n >= max_per {
            continue;
        }
        *n += 1;
        out.push(h);
    }
    out
}

fn attach_meta(mut hits: Vec<SearchHit>, mem: &MemIndex) -> Vec<SearchHit> {
    for h in &mut hits {
        if h.origin.is_none() {
            h.meta = mem.meta.get(&h.source).cloned();
        }
    }
    hits
}

fn allowed_sources(mem: &MemIndex, filters: &SearchFilters) -> HashSet<String> {
    let mut sources: Vec<&str> = mem.chunks.iter().map(|c| c.source_path.as_str()).collect();
    sources.sort_unstable();
    sources.dedup();
    sources
        .into_iter()
        .filter(|src| {
            filters.matches(
                src,
                mem.meta.get(*src),
                mem.modified.get(*src).map(String::as_str),
            )
        })
        .map(|s| s.to_string())
        .collect()
}

fn cosine_at(mem: &MemIndex, qv: &[f32], i: usize) -> f32 {
    crate::core::vector::cosine_similarity(qv, &mem.vectors[i])
}

struct ScoreOpts<'a> {
    qv: &'a [f32],
    qtoks: &'a HashSet<String>,
    allowed: &'a HashSet<String>,
    limit: usize,
    alpha: f32,
    settings: &'a Settings,
    filtered: bool,
}

fn score_hits(mem: &MemIndex, opts: ScoreOpts<'_>) -> Vec<SearchHit> {
    let ScoreOpts {
        qv,
        qtoks,
        allowed,
        limit,
        alpha,
        settings,
        filtered,
    } = opts;
    let mut cos = vec![f32::NEG_INFINITY; mem.chunks.len()];
    for (i, ch) in mem.chunks.iter().enumerate() {
        if allowed.contains(&ch.source_path) {
            cos[i] = cosine_at(mem, qv, i);
        }
    }
    let dense_n = dense_width(limit, settings, filtered);
    let pool = lexical::top_indices(&cos, dense_n.min(mem.chunks.len()));
    let pool: Vec<usize> = pool.into_iter().filter(|&i| cos[i].is_finite()).collect();
    let mut cand = pool.clone();
    if alpha > 0.0 {
        for i in mem.corpus.bm25_global_top(qtoks, settings.search.lex_pool) {
            if allowed.contains(&mem.chunks[i].source_path) && !cand.contains(&i) {
                cand.push(i);
            }
        }
    }
    let bm = mem.corpus.bm25_pool(qtoks, &mem.chunks, &cand);
    let bm25_max = mem.corpus.bm25_max(qtoks);
    let mut hits = Vec::with_capacity(cand.len());
    for (k, &i) in cand.iter().enumerate() {
        let ch = &mem.chunks[i];
        let score = fuse_score(cos[i], bm[k], bm25_max, alpha);
        hits.push(hit_from(ch, score, None));
    }
    sort_hits(&mut hits);
    cap_then(
        hits,
        settings.search.max_per_source,
        limit.saturating_mul(4).max(limit),
    )
}

fn dense_width(limit: usize, settings: &Settings, filtered: bool) -> usize {
    let mut n = settings.search.pool;
    let over = if settings.search.max_per_source > 0 {
        limit.saturating_mul(4)
    } else {
        limit
    };
    n = n.max(over);
    if filtered {
        n = n.saturating_mul(3);
    }
    n.max(1)
}

fn cap_then(hits: Vec<SearchHit>, max_per: usize, keep: usize) -> Vec<SearchHit> {
    let capped = if max_per > 0 {
        cap_per_source(hits, max_per)
    } else {
        hits
    };
    capped.into_iter().take(keep).collect()
}

fn rrf_hits(
    mem: &MemIndex,
    qv: &[f32],
    qtoks: &HashSet<String>,
    allowed: &HashSet<String>,
    limit: usize,
) -> Vec<SearchHit> {
    let n = ranking::MIN_CHANNEL_CANDIDATES.max(limit.saturating_mul(ranking::CHANNEL_OVERFETCH));
    let mut cos = vec![f32::NEG_INFINITY; mem.chunks.len()];
    for (i, ch) in mem.chunks.iter().enumerate() {
        if allowed.contains(&ch.source_path) {
            cos[i] = cosine_at(mem, qv, i);
        }
    }
    let dense = lexical::top_indices(&cos, n.min(mem.chunks.len()));
    let dense: Vec<usize> = dense.into_iter().filter(|&i| cos[i].is_finite()).collect();
    let lex = mem
        .corpus
        .bm25_global_top(qtoks, n)
        .into_iter()
        .filter(|&i| allowed.contains(&mem.chunks[i].source_path))
        .collect::<Vec<_>>();
    let mut scores: HashMap<usize, f32> = HashMap::new();
    for (rank, i) in dense.iter().enumerate() {
        *scores.entry(*i).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }
    for (rank, i) in lex.iter().enumerate() {
        *scores.entry(*i).or_insert(0.0) += FTS_CHANNEL_WEIGHT / (RRF_K + rank as f32 + 1.0);
    }
    let mut hits: Vec<SearchHit> = scores
        .into_iter()
        .map(|(i, score)| hit_from(&mem.chunks[i], score, None))
        .collect();
    sort_hits(&mut hits);
    hits
}

fn hit_from(ch: &DocChunk, score: f32, origin: Option<String>) -> SearchHit {
    SearchHit {
        source: ch.source_path.clone(),
        section: ch.section_title.clone(),
        hierarchy: Some(ch.hierarchy.clone()),
        text: ch.content.clone(),
        start_line: ch.start_line,
        end_line: ch.end_line,
        score: Some(score),
        meta: None,
        origin,
    }
}

enum Outcome {
    Chunks(String, Vec<DocChunk>),
    Empty,
    Quarantine(String),
    Unusable(String),
}

fn ingest_file(
    path: &Path,
    rel: &str,
    scanner: &SecretScanner,
    per_row_dirs: &[String],
) -> Outcome {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let markdown = ext == "md" || ext == "markdown" || ext == "mdx";
    let office = is_convertible(&ext);
    let text = if office {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return Outcome::Unusable(format!("conversion failed: {e}")),
        };
        if let Some(kind) = scanner.scan(rel, &String::from_utf8_lossy(&bytes)) {
            return Outcome::Quarantine(kind);
        }
        let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or(rel);
        match convert::to_markdown(&ext, &bytes, title) {
            Ok(md) => md,
            Err(e) => return Outcome::Unusable(format!("conversion failed: {e}")),
        }
    } else {
        match doctext::read_lossy(path) {
            Some(t) => {
                if t.as_bytes().contains(&0) {
                    return Outcome::Unusable("unreadable encoding".into());
                }
                t
            }
            None => return Outcome::Unusable("unreadable encoding".into()),
        }
    };
    if let Some(kind) = scanner.scan(rel, &text) {
        return Outcome::Quarantine(kind);
    }
    let per_row = (office && ext == "xlsx") || mdchunk::in_dirs(rel, per_row_dirs);
    let chunks = if markdown || office {
        mdchunk::chunk_markdown(rel, &text, per_row)
    } else {
        parse_markdown(rel, &text)
    };
    if chunks.is_empty() {
        Outcome::Empty
    } else {
        Outcome::Chunks(text, chunks)
    }
}

fn embed_input(ch: &DocChunk) -> String {
    format!("{}\n{}", ch.hierarchy.join(CRUMB_SEP), ch.content)
}

pub fn discover(root: &Path, settings: &Settings) -> Result<Vec<String>> {
    discover_with(root, settings, None)
}

fn discover_with(
    root: &Path,
    settings: &Settings,
    mut hooks: Option<&mut dyn IndexHooks>,
) -> Result<Vec<String>> {
    let filter = DiscoveryFilter::new(root, &settings.collect)?;
    let restricted = !settings.collect.extensions.is_empty();
    let mut rels = Vec::new();
    for start in filter.roots() {
        let gate = filter.clone();
        let walk = WalkBuilder::new(&start)
            .standard_filters(filter.respect_gitignore())
            .hidden(false)
            .parents(true)
            .filter_entry(move |e| {
                if e.depth() == 0 {
                    return true;
                }
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    !gate.skip_dir(e.path())
                } else {
                    true
                }
            })
            .build();
        for dent in walk {
            let dent = match dent {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("walk: {e}");
                    continue;
                }
            };
            if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let len = dent.metadata().map(|m| m.len()).unwrap_or(0);
            if filter.skip_file(dent.path(), len) {
                continue;
            }
            if !wanted(dent.path(), restricted) {
                continue;
            }
            if let Ok(rel) = dent.path().strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if !rel.is_empty() {
                    rels.push(rel.clone());
                    if let Some(h) = hooks.as_mut() {
                        h.on_scan(rels.len(), &rel);
                    }
                }
            }
        }
    }
    rels.sort();
    rels.dedup();
    Ok(rels)
}

fn wanted(path: &Path, restricted: bool) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "md" || ext == "markdown" || ext == "mdx" || is_convertible(&ext) {
        return true;
    }
    const TEXT: &[&str] = &[
        "txt",
        "rst",
        "adoc",
        "org",
        "html",
        "htm",
        "yaml",
        "yml",
        "toml",
        "json",
        "jsonc",
        "csv",
        "xml",
        "sql",
        "conf",
        "cfg",
        "ini",
        "env",
        "properties",
        "service",
    ];
    if TEXT.contains(&ext.as_str()) || ext.is_empty() {
        return true;
    }
    restricted
}

fn read_for_meta(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_convertible(&ext) {
        let bytes = std::fs::read(path).ok()?;
        let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        convert::to_markdown(&ext, &bytes, title).ok()
    } else {
        doctext::read_lossy(path)
    }
}

fn file_unchanged(conn: &Connection, rel: &str, path: &Path) -> Result<bool> {
    let meta = std::fs::metadata(path)?;
    let mtime = mtime_millis(&meta);
    let hash = content_hash(path)?;
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT mtime_millis, content_hash FROM doc_file_meta WHERE source_path = ?1",
            [rel],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(row.is_some_and(|(m, h)| m == mtime && h == hash))
}

fn record_file(conn: &Connection, rel: &str, path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    conn.execute(
        "INSERT OR REPLACE INTO doc_file_meta (source_path, mtime_millis, content_hash) VALUES (?1, ?2, ?3)",
        params![rel, mtime_millis(&meta), content_hash(path)?],
    )?;
    Ok(())
}

fn mtime_millis(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn content_hash(path: &Path) -> Result<i64> {
    let bytes = std::fs::read(path)?;
    let dig = Sha256::digest(&bytes);
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&dig[..8]);
    Ok(i64::from_le_bytes(buf))
}

fn ymd_from_millis(ms: i64) -> String {
    let t = UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0) as u64);
    chrono::DateTime::<chrono::Local>::from(t)
        .format("%Y-%m-%d")
        .to_string()
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    let (chunks, _) = b.as_chunks::<4>();
    chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS doc_chunks (
            id TEXT PRIMARY KEY,
            source_path TEXT NOT NULL,
            section_title TEXT NOT NULL,
            heading_level INTEGER NOT NULL,
            content TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            symbol_references TEXT NOT NULL,
            hierarchy TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_doc_chunks_source ON doc_chunks(source_path);
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
            source_path, section_title, content, hierarchy, id UNINDEXED,
            tokenize = 'porter unicode61'
        );
        CREATE TABLE IF NOT EXISTS chunk_vectors (
            chunk_id TEXT PRIMARY KEY,
            vector BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS embed_cache (
            embedder TEXT NOT NULL,
            digest TEXT NOT NULL,
            vector BLOB NOT NULL,
            PRIMARY KEY (embedder, digest)
        );
        CREATE TABLE IF NOT EXISTS doc_file_meta (
            source_path TEXT PRIMARY KEY,
            mtime_millis INTEGER NOT NULL,
            content_hash INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS index_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )?;
    docmeta::init_tables(conn)?;
    parse_issues::init_table(conn)?;
    Ok(())
}

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = crate::core::sqlite::open(path)?;
    init_schema(&conn)?;
    Ok(conn)
}

fn settings_from_conn(conn: &Connection) -> (Settings, Option<PathBuf>) {
    match meta_get(conn, "workspace_root") {
        Some(root) => {
            let path = PathBuf::from(&root);
            let settings = Settings::load(&path).unwrap_or_default();
            (settings, Some(path))
        }
        None => (Settings::default(), None),
    }
}

fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM index_meta WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO index_meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

fn cache_get(conn: &Connection, embedder: &str, digest: &str) -> Result<Option<Vec<f32>>> {
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT vector FROM embed_cache WHERE embedder = ?1 AND digest = ?2",
            params![embedder, digest],
            |r| r.get(0),
        )
        .ok();
    Ok(row.map(|b| blob_to_vec(&b)))
}

fn cache_put(conn: &Connection, embedder: &str, digest: &str, vector: &[f32]) -> Result<()> {
    match conn.execute(
        "INSERT OR REPLACE INTO embed_cache (embedder, digest, vector) VALUES (?1, ?2, ?3)",
        params![embedder, digest, vec_to_blob(vector)],
    ) {
        Ok(_) => Ok(()),
        // A new query writes the embedding cache. Don't fail the search if the
        // daemon refresher currently holds the write lock.
        Err(e) if crate::core::sqlite::is_busy(&e) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn delete_source(conn: &Connection, rel: &str) -> Result<()> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM doc_chunks WHERE source_path = ?1")?;
        let rows = stmt.query_map([rel], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        conn.execute("DELETE FROM chunks_fts WHERE id = ?1", [id])?;
        conn.execute("DELETE FROM chunk_vectors WHERE chunk_id = ?1", [id])?;
    }
    conn.execute("DELETE FROM doc_chunks WHERE source_path = ?1", [rel])?;
    Ok(())
}

fn store_chunks(conn: &Connection, chunks: &[DocChunk]) -> Result<()> {
    for ch in chunks {
        let hierarchy = serde_json::to_string(&ch.hierarchy)?;
        let symbols = serde_json::to_string(&ch.symbol_references)?;
        conn.execute(
            "INSERT OR REPLACE INTO doc_chunks
             (id, source_path, section_title, heading_level, content, start_line, end_line, symbol_references, hierarchy)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ch.id,
                ch.source_path,
                ch.section_title,
                ch.heading_level,
                ch.content,
                ch.start_line as i64,
                ch.end_line as i64,
                symbols,
                hierarchy
            ],
        )?;
        let crumb = ch.hierarchy.join(CRUMB_SEP);
        conn.execute(
            "INSERT INTO chunks_fts (source_path, section_title, content, hierarchy, id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ch.source_path, ch.section_title, ch.content, crumb, ch.id],
        )?;
    }
    Ok(())
}

fn paths_not_in(conn: &Connection, table: &str, active: &HashSet<String>) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("SELECT source_path FROM {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let p = row?;
        if !active.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn dedup_chunks(conn: &Connection, roots: &[String], min_chars: usize) -> Result<Vec<String>> {
    if min_chars == 0 {
        return Ok(Vec::new());
    }
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, source_path, content FROM doc_chunks")?;
        let q = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        q.collect::<rusqlite::Result<_>>()?
    };
    let mut groups: HashMap<String, Vec<(String, String, usize)>> = HashMap::new();
    for (id, source, content) in rows {
        let norm = normalize_ws(&content);
        if norm.chars().count() < min_chars {
            continue;
        }
        let rank = crate::core::collect::rank_in_roots(roots, &source);
        groups.entry(norm).or_default().push((id, source, rank));
    }
    let mut touched = Vec::new();
    for group in groups.values() {
        if group.len() < 2 {
            continue;
        }
        let mut keep = 0;
        for (i, item) in group.iter().enumerate() {
            let best = &group[keep];
            if (item.2, &item.1, &item.0) < (best.2, &best.1, &best.0) {
                keep = i;
            }
        }
        for (i, (id, source, _)) in group.iter().enumerate() {
            if i == keep {
                continue;
            }
            conn.execute("DELETE FROM chunks_fts WHERE id = ?1", [id])?;
            conn.execute("DELETE FROM chunk_vectors WHERE chunk_id = ?1", [id])?;
            conn.execute("DELETE FROM doc_chunks WHERE id = ?1", [id])?;
            touched.push(source.clone());
        }
    }
    touched.sort();
    touched.dedup();
    Ok(touched)
}

fn issues_for(conn: &Connection, rel: &str) -> Result<Vec<ParseIssue>> {
    parse_issues::init_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT file_path, kind, reason, severity FROM parse_issues WHERE file_path = ?1",
    )?;
    let rows = stmt.query_map([rel], |r| {
        Ok(ParseIssue {
            file_path: r.get(0)?,
            kind: r.get(1)?,
            reason: r.get(2)?,
            severity: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Imports an embedding cache (chunks.jsonl aligned with embeddings.npy).
pub fn import_vectors(
    conn: &Connection,
    embedder_id: &str,
    chunks_path: &Path,
    npy_path: &Path,
) -> Result<usize> {
    use ndarray::Array2;
    use ndarray_npy::ReadNpyExt;
    use std::io::BufRead;
    let file =
        std::fs::File::open(npy_path).with_context(|| format!("reading {}", npy_path.display()))?;
    let arr = Array2::<f32>::read_npy(file).context("embeddings.npy unreadable")?;
    let lines: Vec<String> = {
        let f = std::fs::File::open(chunks_path)
            .with_context(|| format!("reading {}", chunks_path.display()))?;
        std::io::BufReader::new(f)
            .lines()
            .collect::<std::io::Result<_>>()?
    };
    let lines: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.len() != arr.nrows() {
        bail!(
            "inconsistent counts: {} lines in chunks.jsonl, {} vectors in embeddings.npy",
            lines.len(),
            arr.nrows()
        );
    }
    let mut n = 0;
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value =
            serde_json::from_str(line).with_context(|| format!("line {}", i + 1))?;
        let crumb = v["breadcrumb"]
            .as_str()
            .or_else(|| v["title"].as_str())
            .unwrap_or("");
        let text = v["text"]
            .as_str()
            .or_else(|| v["content"].as_str())
            .unwrap_or("");
        let input = format!("{crumb}\n{text}");
        let row = arr.row(i);
        let vec: Vec<f32> = row.iter().copied().collect();
        cache_put(conn, embedder_id, &digest(&input), &vec)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lexical::{tokenize, Corpus, K1};

    fn chunk(path: &str, content: &str) -> DocChunk {
        DocChunk {
            id: path.into(),
            source_path: path.into(),
            section_title: "t".into(),
            heading_level: 1,
            hierarchy: vec![],
            content: content.into(),
            start_line: 1,
            end_line: 1,
            symbol_references: vec![],
        }
    }

    #[test]
    fn bm25_matches_the_absolute_formula() {
        let chunks = vec![chunk("xx", "alpha alpha"), chunk("yy", "beta beta beta")];
        let corpus = Corpus::new(&chunks);
        let q: HashSet<String> = tokenize("alpha").into_iter().collect();
        let scores = corpus.bm25_pool(&q, &chunks, &[0]);
        let n = 2.0f32;
        let df = 1.0f32;
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        let dl = 3.0f32;
        let avgdl = 3.5f32;
        let f = 2.0f32;
        let expect = idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - 0.75 + 0.75 * dl / avgdl));
        assert!(
            (scores[0] - expect).abs() < 1e-4,
            "{} vs {expect}",
            scores[0]
        );
        let max = corpus.bm25_max(&q);
        assert!((max - idf * (K1 + 1.0)).abs() < 1e-4);
    }

    #[test]
    fn local_hash_alpha_lets_bm25_beat_hash_cosine() {
        // α ≥ 1.5: a perfect BM25 hit outranks a high hash cosine with weak BM25.
        let hash_noise = fuse_score(0.9, 0.1, 1.0, 1.5);
        let lexical_hit = fuse_score(0.2, 1.0, 1.0, 1.5);
        assert!(lexical_hit > hash_noise, "{lexical_hit} vs {hash_noise}");
    }

    #[test]
    fn fusion_uses_absolute_ceiling() {
        assert!((fuse_score(0.4, 3.0, 6.0, 0.5) - 0.65).abs() < 1e-6);
        assert!((fuse_score(0.4, 3.0, 0.0, 0.5) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn search_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.md"),
            "# A\n\n## Port\n\nLe service écoute sur le port 4100.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.md"),
            "# B\n\n## Autre\n\nRien à voir avec ce sujet précis.\n",
        )
        .unwrap();
        let mut engine = DocEngine::in_memory().unwrap();
        engine.sync_directory_incremental(dir.path(), None).unwrap();
        let a = engine.search("port 4100", 5).unwrap();
        let b = engine.search("port 4100", 5).unwrap();
        let ids: Vec<_> = a
            .iter()
            .map(|h| (h.source.clone(), h.start_line, h.score))
            .collect();
        let ids2: Vec<_> = b
            .iter()
            .map(|h| (h.source.clone(), h.start_line, h.score))
            .collect();
        assert_eq!(ids, ids2);
        assert_eq!(a[0].source, "a.md");
    }
}
