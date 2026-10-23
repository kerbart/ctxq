//! Subcommands of the documentation CLI.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::config::ProjectPaths;
use crate::cli::daemon::{self, format_results, legacy_json};
use crate::core::agent::{self, SearchArgs};
use crate::core::docmeta::SearchFilters;
use crate::core::hybrid::{self, DocEngine};
use crate::core::ingest::{self, IngestOptions, MapOptions};
use crate::core::settings::Settings;
use crate::core::settings::StoreMode;
use crate::core::store;
use crate::core::vector::Embedder;

/// Syncs the graph into `docs.db` when `[code] enabled` (default: true).
pub(crate) fn sync_code_graph(
    paths: &ProjectPaths,
    settings: &Settings,
) -> Result<Option<crate::code::sync::SyncReport>> {
    if !settings.code.enabled {
        return Ok(None);
    }
    let mut store = crate::code::store::CodeStore::open(&paths.docs_db)?;
    let rules = crate::code::sync::IgnoreRules {
        respect_gitignore: settings.collect.respect_gitignore,
        custom_ignore_files: settings
            .collect
            .exclude_globs_file
            .iter()
            .cloned()
            .collect(),
        max_file_bytes: settings.collect.max_file_bytes,
    };
    Ok(Some(crate::code::sync::sync_tree(
        &mut store,
        &paths.root_dir,
        &rules,
    )?))
}

fn paths(target: Option<&Path>) -> Result<ProjectPaths> {
    ProjectPaths::discover(target)
}

fn open_existing(target: Option<&Path>, embedder: Embedder) -> Result<(ProjectPaths, DocEngine)> {
    let paths = paths(target)?;
    if !paths.docs_db.exists() {
        bail!(
            "no index for {}; run `documentum sync`",
            paths.root_dir.display()
        );
    }
    let engine = DocEngine::open_with_embedder(&paths.docs_db, embedder)?;
    Ok((paths, engine))
}

fn print_sync(report: &hybrid::SyncReport) {
    eprintln!(
        "Done: {} files seen, {} chunks, {} files resumed, {} empty, {} quarantined, {} unusable.",
        report.files,
        report.chunks,
        report.changed,
        report.empty,
        report.quarantined,
        report.unusable
    );
}

pub fn run_index(target: Option<&Path>, embedder: Embedder) -> Result<()> {
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    eprintln!("1. Ouverture de l'index {}", paths.docs_db.display());
    let mut engine = DocEngine::open_with_embedder(&paths.docs_db, embedder)?;
    eprintln!("2. Reindexing {}", paths.root_dir.display());
    let report = engine.reindex_directory(
        &paths.root_dir,
        Some(|i: usize, _changed: bool| {
            if i.is_multiple_of(100) {
                eprintln!("  files processed: {i}");
            }
        }),
        None::<fn(usize, usize)>,
        None::<fn(usize, usize)>,
    )?;
    eprintln!("3. Embeddings and database up to date");
    if let Some(code) = sync_code_graph(&paths, &settings)? {
        eprintln!(
            "4. Code graph: {} files seen, {} extracted, {} unchanged",
            code.scanned, code.extracted, code.skipped_unchanged
        );
    }
    print_sync(&report);
    Ok(())
}

pub fn run_sync(target: Option<&Path>, embedder: Embedder) -> Result<()> {
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    eprintln!("1. Ouverture de l'index {}", paths.docs_db.display());
    let mut engine = DocEngine::open_with_embedder(&paths.docs_db, embedder)?;
    eprintln!("2. Synchronisation de {}", paths.root_dir.display());
    let report = engine.sync_directory_incremental(
        &paths.root_dir,
        Some(|i: usize, _changed: bool| {
            if i.is_multiple_of(100) {
                eprintln!("  files processed: {i}");
            }
        }),
        None::<fn(usize, usize)>,
        None::<fn(usize, usize)>,
    )?;
    eprintln!("3. Embeddings and database up to date");
    if let Some(code) = sync_code_graph(&paths, &settings)? {
        eprintln!(
            "4. Code graph: {} files seen, {} extracted, {} unchanged",
            code.scanned, code.extracted, code.skipped_unchanged
        );
    }
    print_sync(&report);
    Ok(())
}

pub struct SearchRequest<'a> {
    pub target: Option<&'a Path>,
    pub query: &'a str,
    pub limit: usize,
    pub full: bool,
    pub json_out: bool,
    pub no_daemon: bool,
    pub filters: &'a SearchFilters,
}

/// One search command: through the resident daemon when it can be reached (started on
/// demand, index stays warm), otherwise in-process. Same results either way.
pub fn run_search(
    req: SearchRequest<'_>,
    embedder: impl FnOnce() -> Result<Embedder>,
) -> Result<()> {
    let SearchRequest {
        target,
        query,
        limit,
        full,
        json_out,
        no_daemon,
        filters,
    } = req;
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    let fr = settings.scan.lang.eq_ignore_ascii_case("fr");
    let emit = |data: &Value| {
        if json_out {
            println!("{data}");
        } else {
            print!("{}", format_results(query, data, full, fr));
        }
    };
    if !no_daemon {
        let ask = serde_json::json!({
            "op": "search",
            "query": query,
            "limit": limit,
            "tags": filters.tags,
            "source_types": filters.source_types,
            "since": filters.since,
            "path_prefixes": filters.path_prefixes,
        });
        if let Some(resp) = daemon::ask(&paths, &settings, &ask, true) {
            if resp["ok"].as_bool() != Some(true) {
                bail!("{}", resp["error"].as_str().unwrap_or("daemon error"));
            }
            emit(&resp["data"]);
            return Ok(());
        }
    }
    let (paths, engine) = open_existing(target, embedder()?)?;
    let value = agent::search(
        &engine,
        &paths.root_dir,
        &SearchArgs {
            query: query.to_string(),
            limit,
            filters: filters.clone(),
            max_chars: 0,
        },
    )?;
    emit(&value);
    Ok(())
}

/// `daemon --stop`.
pub fn run_daemon_stop(target: Option<&Path>) -> Result<()> {
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    let stopped = daemon::stop_daemon(&paths, &settings)?;
    println!(
        "{}",
        if stopped {
            "daemon stopped"
        } else {
            "no daemon running"
        }
    );
    Ok(())
}

pub fn run_agent_op(
    target: Option<&Path>,
    req: Value,
    embedder: impl FnOnce() -> Result<Embedder>,
) -> Result<()> {
    let (paths, engine) = open_existing(target, embedder()?)?;
    let data = match req["op"].as_str().unwrap_or("") {
        "inventory" => agent::inventory(
            &engine,
            &paths.root_dir,
            req["kind"].as_str(),
            req["filter"].as_str(),
            req["limit"].as_u64().unwrap_or(50) as usize,
        )?,
        "map" => agent::map(
            &engine,
            &paths.root_dir,
            req["prefix"].as_str(),
            req["depth"].as_u64().unwrap_or(2) as usize,
            req["limit"].as_u64().unwrap_or(200) as usize,
        )?,
        "status" => agent::status(&engine, &paths.root_dir)?,
        "annotate" => {
            let tags: Vec<String> = req
                .get("tags")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let path = req["path"].as_str().context("chemin requis")?;
            agent::annotate(
                &engine,
                path,
                req["summary"].as_str(),
                &tags,
                req["author"].as_str().unwrap_or("cli"),
            )?
        }
        other => bail!("unknown operation {other}"),
    };
    println!("{data}");
    Ok(())
}

pub fn run_doctor(target: Option<&Path>, workspaces: &[String], all: bool) -> Result<()> {
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    if workspaces.is_empty() {
        let p = paths(target)?;
        roots.push(("default".into(), p.root_dir));
    } else {
        let take = if all { workspaces.len() } else { 1 };
        for spec in workspaces.iter().take(take) {
            let (name, path) = spec.split_once('=').context("--workspace NOM=CHEMIN")?;
            roots.push((name.to_string(), PathBuf::from(path)));
        }
    }
    for (name, root) in roots {
        println!("# doctor {name} ({})", root.display());
        let db = ProjectPaths::discover(Some(&root))?.docs_db;
        if !db.exists() {
            println!("index : absent ({})", db.display());
            continue;
        }
        let engine = DocEngine::open(&db)?;
        let check: String = engine
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error".into());
        println!("integrity: {check}");
        let c = engine.completeness(&root)?;
        println!(
            "coverage: indexed {} · empty {} · quarantined {} · unusable {} · unaccounted {}",
            c.indexed,
            c.empty,
            c.quarantined.len(),
            c.unusable.len(),
            c.unaccounted.len()
        );
        for (p, why) in c.quarantined.iter().chain(c.unusable.iter()) {
            println!("  - {p} : {why}");
        }
        for p in &c.unaccounted {
            println!("  - perdu : {p}");
        }
        let last = engine
            .conn()
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'last_sync'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_else(|_| "jamais".into());
        println!("dernier sync : {last}");
        if c.unaccounted.is_empty() {
            println!("no document lost");
        }
    }
    Ok(())
}

pub fn run_stats(workspace: Option<&str>, json_out: bool, purge: bool) -> Result<()> {
    let base = std::env::var_os("DOCUMENTUM_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .map(|p| p.join("documentum"))
        })
        .context("cache introuvable")?;
    let db = base.join("telemetry.db");
    if purge {
        if db.exists() {
            std::fs::remove_file(&db)?;
        }
        println!("telemetry deleted ({})", db.display());
        return Ok(());
    }
    let value = json!({
        "workspace": workspace,
        "calls": 0,
        "success": 0,
        "failure": 0,
        "duration_ms": 0,
        "tokens_estimated": 0,
        "note": "no MCP call recorded",
    });
    if json_out {
        println!("{value}");
    } else {
        println!("telemetry: 0 calls");
    }
    Ok(())
}

pub fn run_scan(target: Option<&Path>, only: &[String]) -> Result<()> {
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    let reports = crate::core::sources::scan(&settings, &paths.root_dir, only)?;
    for r in &reports {
        match &r.error {
            Some(e) => eprintln!("source {}: failed ({e}); previous document kept", r.name),
            None if r.changed => eprintln!("source {}: wrote {}", r.name, r.path.display()),
            None => eprintln!("source {}: unchanged", r.name),
        }
    }
    Ok(())
}

pub fn run_store(action: &str, target: Option<&Path>, embedder: Embedder) -> Result<()> {
    let (paths, engine) = open_existing(target, embedder)?;
    let settings = engine.settings();
    match action {
        "status" => match engine.primary_store()? {
            Some(store) => {
                let local: i64 =
                    engine
                        .conn()
                        .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))?;
                println!(
                    "backend = {}: {} points in the store, {local} chunks in the index",
                    store.label(),
                    store.count().unwrap_or(0)
                );
            }
            None => println!("backend = local: vectors in {}", paths.docs_db.display()),
        },
        "push" => {
            if settings.store.mode != StoreMode::Owned {
                bail!("store is read-only (attached mode): push refused");
            }
            let Some(store) = store::open_primary(&settings.store)? else {
                bail!("[store] is local: nothing to push");
            };
            let mut stmt = engine.conn().prepare(
                "SELECT c.id, c.source_path, c.content, c.start_line, c.end_line, c.hierarchy, v.vector
                 FROM doc_chunks c JOIN chunk_vectors v ON v.chunk_id = c.id",
            )?;
            let rows = stmt.query_map([], |r| {
                let hierarchy: String = r.get(5)?;
                let crumbs: Vec<String> = serde_json::from_str(&hierarchy).unwrap_or_default();
                let blob: Vec<u8> = r.get(6)?;
                Ok(crate::core::store::StorePoint {
                    chunk_id: r.get(0)?,
                    source: r.get(1)?,
                    text: r.get(2)?,
                    start_line: r.get::<_, i64>(3)? as usize,
                    end_line: r.get::<_, i64>(4)? as usize,
                    breadcrumb: crumbs.join(crate::core::mdchunk::CRUMB_SEP),
                    vector: {
                        let (chunks, _) = blob.as_chunks::<4>();
                        chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
                    },
                })
            })?;
            let points: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
            if points.is_empty() {
                println!("no vector to push");
                return Ok(());
            }
            store.ensure(points[0].vector.len())?;
            for batch in points.chunks(128) {
                store.upsert(batch)?;
            }
            println!("{} points pushed to {}", points.len(), store.label());
        }
        other => bail!("action store inconnue : {other}"),
    }
    Ok(())
}

pub fn run_ingest(intake: &Path, context: &Path, force: bool, json_out: bool) -> Result<()> {
    let report = ingest::ingest(&IngestOptions {
        intake: intake.to_path_buf(),
        context: context.to_path_buf(),
        force,
    })?;
    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!(
            "kept {} · duplicates {} · repos {} · converted {} · copied {} · skipped {} · quarantined {} · failures {}",
            report.kept,
            report.discarded_duplicates,
            report.repos,
            report.converted,
            report.copied,
            report.skipped,
            report.quarantined,
            report.failed
        );
        println!("staging : {}", report.staging.display());
        for (p, why) in &report.quarantine {
            println!("quarantaine : {p} ({why})");
        }
        for (p, why) in &report.failures {
            println!("failed: {p} ({why})");
        }
    }
    Ok(())
}

pub fn run_export_map(
    context: &Path,
    profile: &str,
    intake: &str,
    force: bool,
    agents_template: Option<&Path>,
    json_out: bool,
) -> Result<()> {
    let report = ingest::export_map(&MapOptions {
        context: context.to_path_buf(),
        profile: profile.to_string(),
        intake: intake.to_string(),
        force,
        agents_template: agents_template.map(Path::to_path_buf),
    })?;
    if json_out {
        println!(
            "{}",
            json!({
                "written": report.written,
                "docs": report.docs,
                "categories": report.categories,
                "leftover_staging": report.leftover_staging,
            })
        );
    } else if report.written {
        println!(
            "wrote {} ({} docs, {} categories)",
            report.index_md.display(),
            report.docs,
            report.categories
        );
    } else {
        eprintln!(
            "staging is not empty ({} documents); sort them or pass --force",
            report.leftover_staging.len()
        );
        std::process::exit(1);
    }
    if !report.written && json_out {
        std::process::exit(1);
    }
    Ok(())
}

pub fn run_import_vectors(
    chunks: &Path,
    vectors: &Path,
    target: Option<&Path>,
    embedder: Embedder,
) -> Result<()> {
    let paths = paths(target)?;
    let id = embedder.id();
    let engine = DocEngine::open_with_embedder(&paths.docs_db, embedder)?;
    let n = hybrid::import_vectors(engine.conn(), &id, chunks, vectors)?;
    println!("{n} vectors imported into the embedding cache");
    Ok(())
}

pub struct QRequest<'a> {
    pub target: Option<&'a Path>,
    pub query: Option<&'a str>,
    pub k: usize,
    pub full: bool,
    pub json_out: bool,
    pub stop: bool,
    pub no_daemon: bool,
    pub filters: &'a SearchFilters,
}

pub fn run_q(req: QRequest<'_>, embedder: impl FnOnce() -> Result<Embedder>) -> Result<()> {
    let QRequest {
        target,
        query,
        k,
        full,
        json_out,
        stop,
        no_daemon,
        filters,
    } = req;
    let paths = paths(target)?;
    let settings = Settings::load(&paths.root_dir)?;
    if stop {
        let stopped = daemon::stop_daemon(&paths, &settings)?;
        println!(
            "{}",
            if stopped {
                "daemon stopped"
            } else {
                "no daemon running"
            }
        );
        return Ok(());
    }
    let Some(query) = query else {
        bail!("question manquante");
    };
    let req = serde_json::json!({
        "op": "search",
        "query": query,
        "limit": k,
        "tags": filters.tags,
        "source_types": filters.source_types,
        "since": filters.since,
        "path_prefixes": filters.path_prefixes,
    });
    let fr = settings.scan.lang.eq_ignore_ascii_case("fr");
    if !no_daemon {
        if let Some(resp) = daemon::ask(&paths, &settings, &req, true) {
            if resp["ok"].as_bool() != Some(true) {
                bail!("{}", resp["error"].as_str().unwrap_or("daemon error"));
            }
            emit_q(query, &resp["data"], full, json_out, fr);
            return Ok(());
        }
    }
    let (paths, engine) = open_existing(target, embedder()?)?;
    let data = agent::search(
        &engine,
        &paths.root_dir,
        &SearchArgs {
            query: query.to_string(),
            limit: k,
            filters: filters.clone(),
            max_chars: 0,
        },
    )?;
    emit_q(query, &data, full, json_out, fr);
    Ok(())
}

fn emit_q(query: &str, data: &Value, full: bool, json_out: bool, fr: bool) {
    if json_out {
        println!("{}", legacy_json(data));
    } else {
        print!("{}", format_results(query, data, full, fr));
    }
}
