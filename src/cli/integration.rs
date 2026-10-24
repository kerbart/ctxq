//! Commands that tie together the document core, the code graph and the MCP server:
//! `serve`, `setup`, `changes`, `explore`, `context`.

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::commands;
use crate::cli::config::{self, ProjectPaths};
use crate::code::query::{self, SymbolHit};
use crate::code::store::CodeStore;
use crate::core::hybrid::DocEngine;
use crate::core::settings::{EmbedBackend, Settings};
use crate::core::vector::Embedder;
use crate::mcp::budget;
use crate::mcp::docs_engine::WorkspaceDocsBackend;
use crate::mcp::server::{McpServer, WorkspaceConfig};
use crate::mcp::tools::docs;
use crate::setup::apply::Outcome;
use crate::setup::rules::{self, agents_md_body, pointer_body};
use crate::setup::clients::path_has_bin;
use crate::setup::embed::{
    config_path, parse_embed_menu_line, parse_embed_spec, render_embed_menu, write_config,
    EmbedChoice, EmbedWrite,
};
use crate::setup::prompt::{parse_menu_line, render_menu, setup_menu};
use crate::setup::{apply_client, client_by_id, detect_setup_clients, ClientSpec, Scope, CLIENTS};

const SKILL_MD: &str = include_str!("templates/ctxq-investigate.SKILL.md");

struct Loaded {
    configs: Vec<WorkspaceConfig>,
    engines: Vec<(PathBuf, DocEngine)>,
}

fn load_workspaces(target: Option<&Path>, specs: &[String], embedder: Embedder) -> Result<Loaded> {
    let entries = if specs.is_empty() {
        let paths = ProjectPaths::discover(target)?;
        vec![("default".to_string(), paths)]
    } else {
        let mut entries = Vec::new();
        for spec in specs {
            let (name, path) = spec
                .split_once('=')
                .with_context(|| format!("--workspace NAME=PATH, got {spec:?}"))?;
            if name.is_empty() || path.is_empty() {
                bail!("--workspace NAME=PATH, got {spec:?}");
            }
            entries.push((
                name.to_string(),
                ProjectPaths::discover(Some(Path::new(path)))?,
            ));
        }
        entries
    };

    let mut configs = Vec::new();
    let mut engines = Vec::new();
    for (name, paths) in entries {
        if !paths.docs_db.exists() {
            // Start anyway (an MCP client that sees the server die gives up); `status` says
            // the index is empty and `code_sync` / `ctxq sync` fill it.
            eprintln!(
                "ctxq: no index yet for {} ({}); run `ctxq sync` to fill it",
                name,
                paths.root_dir.display()
            );
            if let Some(dir) = paths.docs_db.parent() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let engine = DocEngine::open_with_embedder(&paths.docs_db, embedder.clone())?;
        let code = CodeStore::open(&paths.docs_db)?;
        engines.push((paths.root_dir.clone(), engine));
        configs.push(WorkspaceConfig {
            name,
            root: paths.root_dir,
            code,
        });
    }
    Ok(Loaded { configs, engines })
}

fn one_server(target: Option<&Path>, embedder: Embedder) -> Result<McpServer> {
    let loaded = load_workspaces(target, &[], embedder)?;
    let backend = Arc::new(WorkspaceDocsBackend::new(loaded.engines));
    McpServer::new(loaded.configs, backend)
}

/// MCP server over stdio. stdout is reserved for the protocol; logs go to stderr.
pub fn run_serve(target: Option<&Path>, workspaces: &[String], embedder: Embedder) -> Result<()> {
    let loaded = load_workspaces(target, workspaces, embedder)?;
    let names: Vec<String> = loaded.configs.iter().map(|w| w.name.clone()).collect();
    let backend = Arc::new(WorkspaceDocsBackend::new(loaded.engines));
    let server = McpServer::new(loaded.configs, backend)?;
    eprintln!("ctxq serve (stdio) — workspaces : {}", names.join(", "));
    let stdin = io::stdin();
    let stdout = io::stdout();
    server.serve(stdin.lock(), stdout.lock())?;
    Ok(())
}

pub struct SetupOptions<'a> {
    pub target: Option<&'a Path>,
    pub clients: &'a [String],
    pub uninstall: bool,
    pub command: Option<&'a Path>,
    pub rules: bool,
    pub skill: bool,
    pub hooks: bool,
    pub in_repo: bool,
    pub force_home: bool,
    pub interactive: bool,
    /// Persists `[embed]` before indexing. `ollama` is qwen3-embedding:0.6b.
    pub embed: Option<&'a str>,
    /// Global `--embeddings`: this run only, not written.
    pub backend_override: Option<EmbedBackend>,
    pub embedder: Embedder,
}

pub fn run_setup(opts: SetupOptions<'_>) -> Result<()> {
    let root = match opts.target {
        Some(path) => std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        None => std::env::current_dir().context("current directory")?,
    };
    if !opts.uninstall {
        config::refuse_home(&root, opts.force_home)?;
    }
    if opts.in_repo {
        std::fs::create_dir_all(root.join(".ctxq"))?;
    }
    // Provider first: a different embedder rebuilds vectors, so the choice
    // has to land in ctxq.toml before index/sync. Both questions run before
    // that work, while the user is still at the keyboard.
    let embed_choice = if opts.uninstall {
        None
    } else {
        resolve_embed_choice(&opts, &root)?
    };
    if let Some(choice) = &embed_choice {
        persist_embed(&root, choice)?;
    }
    let chosen = chosen_client_ids(&opts, &root)?;
    let embedder = if opts.uninstall {
        opts.embedder.clone()
    } else {
        let settings = Settings::load(&root)?;
        if embed_choice.is_none() {
            println!("{}", current_embed_line(&settings));
            if !opts.interactive && settings.embed.backend == EmbedBackend::Local {
                println!(
                    "For Ollama and qwen3-embedding: ctxq setup --embed ollama"
                );
            }
        }
        if let Some(over) = opts.backend_override {
            if over != settings.embed.backend {
                println!(
                    "this run uses --embeddings {} (ctxq.toml is unchanged by that flag)",
                    backend_name(over)
                );
            }
        }
        Embedder::from_settings(&settings, opts.backend_override)?
    };
    let paths = ProjectPaths::discover(Some(&root))?;
    if !opts.uninstall {
        // A second setup is how you add a CLI. Rebuild only when there is no
        // index yet; otherwise the incremental sync is the whole point.
        // A provider change still re-embeds: the stored embedder id no longer matches.
        if paths.docs_db.is_file() {
            commands::run_sync(Some(&paths.root_dir), embedder, opts.force_home)?;
        } else {
            commands::run_index(Some(&paths.root_dir), embedder, opts.force_home)?;
        }
    }

    let selected = resolve_clients(&paths.root_dir, &chosen, !opts.interactive)?;
    let command = match opts.command {
        Some(path) => path.to_string_lossy().into_owned(),
        None => std::env::current_exe()
            .context("current binary")?
            .to_string_lossy()
            .into_owned(),
    };
    let repo = paths.root_dir.to_string_lossy().into_owned();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_dir = paths.cache_dir.join("backups");
    let home = home_dir();

    if selected.is_empty() {
        println!("no client to register");
    }
    let mut registered_codex = false;
    for spec in &selected {
        let (base, repo_root) = match spec.scope {
            Scope::Project => (paths.root_dir.clone(), Some(repo.as_str())),
            Scope::User => {
                let home = home
                    .as_deref()
                    .context("HOME is not set: cannot write a user config")?;
                (home.to_path_buf(), Some(repo.as_str()))
            }
        };
        let outcome = apply_client(
            spec,
            &base,
            &backup_dir,
            &timestamp,
            &command,
            repo_root,
            opts.uninstall,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        println!("{} : {}", spec.id, outcome_line(&outcome));
        if spec.id == "codex" || spec.id == "codex-user" {
            registered_codex = true;
        }
        if let Outcome::Rejected(reason) = &outcome {
            bail!("{} : {reason}", spec.id);
        }
    }
    if registered_codex && !opts.uninstall {
        println!(
            "Codex: restart the session, then /mcp. Do not run `ctxq` in the Codex shell — the bwrap sandbox blocks it; MCP is the path that works."
        );
    }

    if opts.rules && !opts.uninstall {
        install_block(
            &paths.root_dir.join("AGENTS.md"),
            &agents_md_body(),
            &backup_dir,
            &timestamp,
        )?;
        for (id, relative, pointer) in pointer_files(&selected) {
            let path = paths.root_dir.join(relative);
            if install_block(&path, &pointer_body(pointer), &backup_dir, &timestamp)? {
                println!("{id} : pointeur {relative}");
            }
        }
    }

    if opts.skill {
        for relative in [
            ".claude/skills/ctxq-investigate/SKILL.md",
            ".agents/skills/ctxq-investigate/SKILL.md",
        ] {
            let path = paths.root_dir.join(relative);
            if opts.uninstall {
                if path.is_file() {
                    std::fs::remove_file(&path)?;
                    println!("skill removed: {relative}");
                }
            } else {
                crate::setup::backup::write_with_backup(&path, SKILL_MD, &backup_dir, &timestamp)?;
                println!("skill installed: {relative}");
            }
        }
    }

    if opts.hooks {
        install_hook(&paths.root_dir, &command, opts.uninstall)?;
    }
    Ok(())
}

fn outcome_line(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Installed { .. } => "registered",
        Outcome::AlreadyUpToDate => "already up to date",
        Outcome::Uninstalled { .. } => "removed",
        Outcome::NothingToUninstall => "nothing to remove",
        Outcome::Rejected(_) => "rejected",
    }
}

fn pointer_files(clients: &[&ClientSpec]) -> Vec<(&'static str, &'static str, &'static str)> {
    let mut out = Vec::new();
    for spec in clients {
        let row = match spec.id {
            "claude" | "generic" => Some(("claude", "CLAUDE.md", "@AGENTS.md")),
            "gemini" => Some(("gemini", "GEMINI.md", "@./AGENTS.md")),
            "vscode" => Some(("vscode", ".github/copilot-instructions.md", "@AGENTS.md")),
            _ => None,
        };
        if let Some(row) = row {
            if !out.iter().any(|(_, rel, _)| *rel == row.1) {
                out.push(row);
            }
        }
    }
    out
}

fn resolve_embed_choice(opts: &SetupOptions<'_>, root: &Path) -> Result<Option<EmbedChoice>> {
    if let Some(spec) = opts.embed {
        return parse_embed_spec(spec)
            .map(Some)
            .map_err(|e| anyhow::anyhow!(e));
    }
    if !opts.interactive {
        return Ok(None);
    }
    let settings = Settings::load(root)?;
    let backend = backend_name(settings.embed.backend);
    let label = match settings.embed.backend {
        EmbedBackend::Ollama | EmbedBackend::OpenAi => {
            format!("{backend} {}", settings.embed_model())
        }
        EmbedBackend::Onnx => "onnx".into(),
        EmbedBackend::Local => "local-hash".into(),
    };
    let menu = render_embed_menu(backend, &label);
    if !io::stdin().is_terminal() {
        bail!(
            "--interactive needs a terminal.\n{menu}\nPass --embed instead (local, ollama, openai, onnx)."
        );
    }
    println!("{menu}");
    print!("> ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    parse_embed_menu_line(&line).map_err(|e| anyhow::anyhow!(e))
}

fn persist_embed(root: &Path, choice: &EmbedChoice) -> Result<()> {
    let ctxq_config = std::env::var_os("CTXQ_CONFIG").map(PathBuf::from);
    let path = config_path(root, ctxq_config.as_deref());
    let wrote = write_config(&path, choice).map_err(|e| anyhow::anyhow!(e))?;
    let shown = display_config(root, &path);
    match wrote {
        EmbedWrite::SkippedDefault => println!("embedder: local (default)"),
        EmbedWrite::Unchanged => println!("embedder: {} (unchanged)", choice.summary()),
        EmbedWrite::Written => {
            println!("embedder: {} → {shown}", choice.summary());
            println!("this setup re-embeds if that provider differs from the index.");
        }
    }
    Ok(())
}

fn current_embed_line(settings: &Settings) -> String {
    match settings.embed.backend {
        EmbedBackend::Local => "embedder: local-hash".into(),
        EmbedBackend::Onnx => "embedder: onnx".into(),
        EmbedBackend::Ollama | EmbedBackend::OpenAi => format!(
            "embedder: {} {}",
            backend_name(settings.embed.backend),
            settings.embed_model()
        ),
    }
}

fn backend_name(backend: EmbedBackend) -> &'static str {
    match backend {
        EmbedBackend::Local => "local",
        EmbedBackend::Onnx => "onnx",
        EmbedBackend::Ollama => "ollama",
        EmbedBackend::OpenAi => "openai",
    }
}

fn display_config(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn chosen_client_ids(opts: &SetupOptions<'_>, root: &Path) -> Result<Vec<String>> {
    if !opts.interactive {
        return Ok(opts.clients.to_vec());
    }
    if !opts.clients.is_empty() {
        bail!("pass either --client or --interactive, not both");
    }
    let menu = setup_menu(root, home_dir().as_deref(), path_has_bin);
    if !io::stdin().is_terminal() {
        bail!(
            "--interactive needs a terminal.\n{}\nPass --client instead ({}).",
            render_menu(&menu),
            CLIENTS.iter().map(|c| c.id).collect::<Vec<_>>().join(", ")
        );
    }
    println!("{}", render_menu(&menu));
    print!("> ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let ids = parse_menu_line(&line, &menu).map_err(|e| anyhow::anyhow!(e))?;
    Ok(ids.into_iter().map(str::to_string).collect())
}

fn resolve_clients(
    root: &Path,
    requested: &[String],
    fallback_all: bool,
) -> Result<Vec<&'static ClientSpec>> {
    if requested.is_empty() {
        return Ok(if fallback_all {
            detect_setup_clients(root, home_dir().as_deref())
        } else {
            Vec::new()
        });
    }
    let requested: Vec<String> = requested.to_vec();
    let known = CLIENTS.iter().map(|c| c.id).collect::<Vec<_>>().join(", ");
    let mut out: Vec<&ClientSpec> = Vec::new();
    for id in &requested {
        if id == "all" {
            for spec in detect_setup_clients(root, home_dir().as_deref()) {
                if !out.iter().any(|s| s.id == spec.id) {
                    out.push(spec);
                }
            }
            continue;
        }
        let spec = client_by_id(id)
            .with_context(|| format!("client inconnu {id:?}. Connus : {known}, all"))?;
        if !out.iter().any(|s| s.id == spec.id) {
            out.push(spec);
        }
    }
    Ok(out)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn install_block(path: &Path, body: &str, backup_dir: &Path, timestamp: &str) -> Result<bool> {
    let existing = if path.is_file() {
        Some(std::fs::read_to_string(path)?)
    } else {
        None
    };
    let (text, changed) = rules::upsert_block(existing.as_deref(), body);
    if changed {
        crate::setup::backup::write_with_backup(path, &text, backup_dir, timestamp)?;
        println!("rules: {}", path.display());
    }
    Ok(changed)
}

fn install_hook(root: &Path, command: &str, uninstall: bool) -> Result<()> {
    let git = root.join(".git");
    if !git.is_dir() {
        bail!("no git repository ({}); hook not installed", git.display());
    }
    let hook = git.join("hooks/post-commit");
    if uninstall {
        if hook.is_file() {
            let text = std::fs::read_to_string(&hook).unwrap_or_default();
            if text.contains("ctxq sync") {
                std::fs::remove_file(&hook)?;
                println!("hook removed: {}", hook.display());
            }
        }
        return Ok(());
    }
    if let Some(parent) = hook.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let script = format!("#!/bin/sh\n# ctxq\n{command:?} sync >/dev/null 2>&1 &\n");
    std::fs::write(&hook, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("hook : {}", hook.display());
    Ok(())
}

pub fn run_changes(
    target: Option<&Path>,
    base: Option<&str>,
    head: Option<&str>,
    depth: u32,
    paths_filter: &[String],
    json_out: bool,
) -> Result<()> {
    let paths = ProjectPaths::discover(target)?;
    if !paths.docs_db.exists() {
        bail!("no index for {}; run `ctxq sync`", paths.root_dir.display());
    }
    let diff = if let Some(base) = base {
        git_diff(&paths.root_dir, base, head.unwrap_or("HEAD"), paths_filter)?
    } else if !io::stdin().is_terminal() {
        let mut raw = String::new();
        io::stdin().read_to_string(&mut raw)?;
        filter_diff(&raw, paths_filter)
    } else {
        git_diff(
            &paths.root_dir,
            "HEAD~1",
            head.unwrap_or("HEAD"),
            paths_filter,
        )?
    };

    let store = CodeStore::open(&paths.docs_db)?;
    let engine = DocEngine::open_with_embedder(&paths.docs_db, Embedder::local())?;
    let depth = depth.clamp(1, 5);
    let report = query::impact(&store, &diff, depth, Some(&engine))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Touched symbols");
    if report.touched.is_empty() {
        println!("  (none)");
    }
    for hit in &report.touched {
        println!("  {}", format_hit(hit));
    }
    println!("Appelants");
    if report.callers.is_empty() {
        println!("  (none)");
    }
    for hit in &report.callers {
        println!("  {}", format_hit(hit));
    }
    println!("Documents");
    if report.doc_refs.is_empty() {
        println!("  (none)");
    }
    for doc in &report.doc_refs {
        println!("  {}:{}  {}", doc.path, doc.line, doc.heading);
    }
    if !report.unresolved_files.is_empty() {
        println!("Fichiers hors graphe");
        for path in &report.unresolved_files {
            println!("  {path}");
        }
    }
    Ok(())
}

fn git_diff(root: &Path, base: &str, head: &str, paths: &[String]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["diff", "-U0", base, head, "--"])
        .args(paths);
    let out = cmd.output().context("git diff")?;
    if !out.status.success() {
        bail!(
            "git diff {base} {head} : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Drops hunks whose `+++` path is outside `paths`. An empty filter keeps the diff.
fn filter_diff(diff: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return diff.to_string();
    }
    let mut out = String::new();
    let mut keep = true;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let raw = rest.trim();
            let path = raw
                .strip_prefix("b/")
                .or_else(|| raw.strip_prefix("a/"))
                .unwrap_or(raw);
            keep = paths.iter().any(|wanted| path_matches(path, wanted));
            if !keep {
                out.push_str("+++ /dev/null\n");
                continue;
            }
        }
        if keep || line.starts_with("diff ") || line.starts_with("--- ") || line.starts_with("+++ ")
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn path_matches(path: &str, wanted: &str) -> bool {
    path == wanted || path.starts_with(&format!("{}/", wanted.trim_end_matches('/')))
}

pub fn run_explore(
    target: Option<&Path>,
    symbol: &str,
    depth: u32,
    max_tokens: u32,
    json_out: bool,
) -> Result<()> {
    let paths = ProjectPaths::discover(target)?;
    if !paths.docs_db.exists() {
        bail!("no index for {}; run `ctxq sync`", paths.root_dir.display());
    }
    let store = CodeStore::open(&paths.docs_db)?;
    let hits = query::find(&store, symbol)?;
    if hits.is_empty() {
        bail!("symbole introuvable : {symbol}");
    }
    if hits.len() > 1 {
        let payload = json!({
            "ambiguous": true,
            "query": symbol,
            "candidates": hits.iter().map(hit_brief).collect::<Vec<_>>(),
        });
        if json_out {
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            println!("Several symbols for {symbol:?}:");
            for hit in &hits {
                println!("  {}", format_hit(hit));
            }
        }
        return Ok(());
    }
    let hit = hits.into_iter().next().expect("len == 1");
    let depth = depth.clamp(1, 5);
    let callers = query::callers(&store, hit.id, depth)?;
    let callees = query::callees(&store, hit.id, depth)?;
    let full = query::body(&paths.root_dir, &hit).unwrap_or_default();
    let lines: Vec<&str> = full.lines().collect();
    let (body, omitted) = budget::truncate_lines(&lines, budget::tokens_to_chars(max_tokens));
    let engine = DocEngine::open(&paths.docs_db).ok();
    let mentioned = engine
        .as_ref()
        .map(|engine| query::doc_links(engine, &hit.name, &hit.qname))
        .unwrap_or_default();

    let payload = json!({
        "symbol": hit.qname,
        "name": hit.name,
        "kind": hit.kind,
        "file": hit.path,
        "line": hit.start_line,
        "end_line": hit.end_line,
        "signature": hit.signature,
        "doc": hit.doc,
        "body": body,
        "body_lines_omitted": omitted,
        "truncated": omitted > 0,
        "callers": callers.edges.iter().map(|e| hit_brief(&e.caller)).collect::<Vec<_>>(),
        "callees": callees.edges.iter().map(|e| hit_brief(&e.callee)).collect::<Vec<_>>(),
        "mentioned_in": mentioned,
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!(
        "{}  {}  {}:{}-{}",
        hit.qname,
        kind_label(hit.kind),
        hit.path,
        hit.start_line,
        hit.end_line
    );
    if !hit.signature.is_empty() {
        println!("signature : {}", hit.signature);
    }
    if !hit.doc.is_empty() {
        println!("doc : {}", hit.doc);
    }
    println!("corps :");
    println!("{body}");
    if omitted > 0 {
        println!("[truncated: {omitted} line(s) omitted; raise --max-tokens]");
    }
    println!("appelants (profondeur {depth}) :");
    print_edges(&callers.edges.iter().map(|e| &e.caller).collect::<Vec<_>>());
    println!("callees (depth {depth}):");
    print_edges(&callees.edges.iter().map(|e| &e.callee).collect::<Vec<_>>());
    if !mentioned.is_empty() {
        println!("mentioned in:");
        for doc in &mentioned {
            println!("  {}:{}  {}", doc.path, doc.line, doc.heading);
        }
    }
    Ok(())
}

fn print_edges(hits: &[&SymbolHit]) {
    if hits.is_empty() {
        println!("  (none)");
        return;
    }
    let mut seen = std::collections::HashSet::new();
    for hit in hits.iter().filter(|h| seen.insert(h.id)) {
        println!("  {}", format_hit(hit));
    }
}

fn format_hit(hit: &SymbolHit) -> String {
    format!(
        "{}  {}  {}:{}",
        hit.qname,
        kind_label(hit.kind),
        hit.path,
        hit.start_line
    )
}

fn hit_brief(hit: &SymbolHit) -> Value {
    json!({
        "symbol": hit.qname,
        "name": hit.name,
        "kind": hit.kind,
        "file": hit.path,
        "line": hit.start_line,
        "signature": hit.signature,
    })
}

fn kind_label(kind: crate::code::model::SymbolKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "symbol".into())
}

pub fn run_context(
    target: Option<&Path>,
    question: &str,
    scope: &str,
    max_tokens: u32,
    page: u32,
    embedder: Embedder,
    json_out: bool,
) -> Result<()> {
    if !["auto", "docs", "code"].contains(&scope) {
        bail!("scope invalide : {scope:?} (auto, docs, code)");
    }
    let server = one_server(target, embedder)?;
    let args = json!({
        "query": question,
        "scope": scope,
        "max_tokens": max_tokens,
        "page": page,
    });
    let outcome = docs::context(&server, &args);
    if outcome.is_error {
        bail!("{}", outcome.text);
    }
    if json_out {
        println!("{}", outcome.text);
        return Ok(());
    }
    let value: Value = serde_json::from_str(&outcome.text).unwrap_or(json!({}));
    let ledger = &value["ledger"];
    println!(
        "page {}/{}  tokens {}/{}  suite : {}",
        ledger["current_page"],
        ledger["total_pages"],
        ledger["tokens_used"],
        ledger["token_budget"],
        if ledger["has_more"].as_bool() == Some(true) {
            "yes"
        } else {
            "no"
        }
    );
    for entry in value["entries"].as_array().into_iter().flatten() {
        println!("{}", "-".repeat(60));
        let kind = entry["type"].as_str().unwrap_or("doc");
        if kind == "code_symbol" {
            println!(
                "code  {}  {}:{}",
                entry["symbol"].as_str().unwrap_or(""),
                entry["file"].as_str().unwrap_or(""),
                entry["line"].as_u64().unwrap_or(0)
            );
            if let Some(sig) = entry["signature"].as_str() {
                println!("{sig}");
            }
        } else {
            println!(
                "doc  {}  {}",
                entry["source"]
                    .as_str()
                    .or_else(|| entry["path"].as_str())
                    .unwrap_or(""),
                entry["title"]
                    .as_str()
                    .or_else(|| entry["section"].as_str())
                    .unwrap_or("")
            );
            if let Some(text) = entry["text"].as_str() {
                println!("{text}");
            }
        }
    }
    Ok(())
}
