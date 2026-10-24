use clap::{Parser, Subcommand, ValueEnum};
use ctxq::cli::commands::{
    run_agent_op, run_daemon_stop, run_doctor, run_export_map, run_import_vectors, run_index,
    run_ingest, run_q, run_scan, run_search, run_stats, run_store, run_sync, QRequest,
    SearchRequest,
};
use ctxq::cli::integration::{
    run_changes, run_context, run_explore, run_serve, run_setup, SetupOptions,
};
use ctxq::core::docmeta::SearchFilters;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ctxq",
    author = "ctxq contributors",
    version = ctxq::VERSION,
    long_version = ctxq::long_version(),
    about = "Ask the index, get the line — hybrid search, inventories, code graph"
)]
struct Cli {
    // Not `global`: `search` and `q` already have a `--path` (filter prefix).
    // The root goes before the subcommand: `ctxq --path ROOT search …`.
    #[arg(short, long, help = "Workspace root (default: current directory)")]
    path: Option<PathBuf>,

    #[arg(long, global = true, value_enum, help = "Overrides [embed].backend")]
    embeddings: Option<EmbeddingsArg>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Full reindex of the documentation")]
    Index {
        /// Allow indexing $HOME (refused by default).
        #[arg(long)]
        force_home: bool,
    },
    #[command(about = "Incremental sync of the documentation")]
    Sync {
        /// Allow indexing $HOME (refused by default).
        #[arg(long)]
        force_home: bool,
    },
    #[command(about = "Index health: integrity, coverage, freshness")]
    Doctor {
        #[arg(long, value_name = "NAME=PATH")]
        workspace: Vec<String>,
        #[arg(long, help = "Inspect every --workspace")]
        all: bool,
    },
    #[command(about = "Keeps the index warm in memory (optional: `search` starts it on its own)")]
    Daemon {
        #[arg(long, help = "HTTP API, e.g. 127.0.0.1:7700")]
        http: Option<String>,
        #[arg(long, help = "Stops this workspace's daemon")]
        stop: bool,
    },
    /// Former name of `search` (legacy JSON output), kept for scripts.
    #[command(hide = true)]
    Q {
        query: Option<String>,
        #[arg(long, default_value_t = 5)]
        k: usize,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        stop: bool,
        #[arg(long)]
        no_daemon: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "type")]
        source_types: Vec<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long = "path")]
        path_prefixes: Vec<String>,
    },
    #[command(about = "Rows of an inventory (JSON)")]
    Inventory {
        kind: Option<String>,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    #[command(about = "Workspace map (JSON)")]
    Map {
        prefix: Option<String>,
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    #[command(about = "Freshness, store, sources, coverage (JSON)")]
    Status,
    #[command(about = "Agent summary or tags; the source file is left unchanged")]
    Annotate {
        path: String,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long, default_value = "cli")]
        author: String,
    },
    #[command(about = "External vector store: push or status")]
    Store {
        #[arg(value_parser = ["push", "status"])]
        action: String,
    },
    #[command(about = "Converts a docx, pptx, xlsx or pdf to markdown")]
    Convert { file: PathBuf },
    #[command(about = "Inventories and converts an intake folder")]
    Ingest {
        #[arg(default_value = "_intake")]
        intake: PathBuf,
        #[arg(long, default_value = "context")]
        context: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(
        name = "export-map",
        about = "Writes INDEX.md, INDEX.json, AGENTS.md, .gitignore"
    )]
    ExportMap {
        #[arg(default_value = "context")]
        context: PathBuf,
        #[arg(long, default_value = "pure-code")]
        profile: String,
        #[arg(long, default_value = "_intake")]
        intake: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        agents_template: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Runs the [[scan.source]] sources")]
    Scan {
        #[arg(long)]
        only: Vec<String>,
        #[arg(long)]
        sync: bool,
    },
    #[command(about = "Seeds the embedding cache (chunks.jsonl + embeddings.npy)")]
    ImportVectors {
        #[arg(long)]
        chunks: PathBuf,
        #[arg(long)]
        vectors: PathBuf,
    },
    #[command(about = "Searches the index: sourced passages, most relevant first")]
    Search {
        query: String,
        #[arg(short, long, visible_alias = "k", default_value_t = 5)]
        limit: usize,
        #[arg(long, help = "Whole passages, no truncation")]
        full: bool,
        #[arg(long, help = "No daemon: opens the index in this process")]
        no_daemon: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "type")]
        source_types: Vec<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long = "path")]
        path_prefixes: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(
        alias = "serve",
        about = "MCP server for agents (launched by the editor, not by you; see `setup`)"
    )]
    Mcp {
        /// Named workspace. Repeatable. The first one is the default. Takes precedence over --path.
        #[arg(long = "workspace", value_name = "NAME=PATH")]
        workspace: Vec<String>,
    },
    #[command(about = "Registers the MCP server with clients")]
    Setup {
        /// Client (`all` or an identifier: claude, cursor, vscode, gemini, codex, …). Repeatable. Default `all` picks project clients and Codex if ~/.codex exists.
        #[arg(long = "client", visible_alias = "agent", value_name = "CLIENT")]
        client: Vec<String>,
        #[arg(long)]
        uninstall: bool,
        #[arg(long)]
        command: Option<PathBuf>,
        /// Writes AGENTS.md and the pointer files. Use `--rules=false` to skip.
        #[arg(long, default_value_t = true, num_args = 0..=1, action = clap::ArgAction::Set)]
        rules: bool,
        #[arg(long)]
        skill: bool,
        #[arg(long)]
        hooks: bool,
        #[arg(long)]
        in_repo: bool,
        /// Allow indexing $HOME (refused by default).
        #[arg(long)]
        force_home: bool,
    },
    #[command(about = "Impact of a diff: symbols, callers, documents")]
    Changes {
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        head: Option<String>,
        #[arg(long, default_value_t = 2)]
        depth: u32,
        #[arg(long)]
        json: bool,
        paths: Vec<String>,
    },
    #[command(about = "A symbol's signature, doc, bounded body, callers and callees")]
    Explore {
        symbol: String,
        #[arg(long, default_value_t = 1)]
        depth: u32,
        #[arg(long, default_value_t = 2000)]
        max_tokens: u32,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Documents and code retrieved together, within a token budget")]
    Context {
        question: String,
        #[arg(long, default_value = "auto")]
        scope: String,
        #[arg(long, default_value_t = 2000)]
        max_tokens: u32,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "MCP call metrics")]
    Stats {
        #[arg(short, long)]
        workspace: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        purge: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmbeddingsArg {
    #[default]
    Local,
    Onnx,
    Ollama,
    Openai,
}

impl From<EmbeddingsArg> for ctxq::core::settings::EmbedBackend {
    fn from(a: EmbeddingsArg) -> Self {
        match a {
            EmbeddingsArg::Local => Self::Local,
            EmbeddingsArg::Onnx => Self::Onnx,
            EmbeddingsArg::Ollama => Self::Ollama,
            EmbeddingsArg::Openai => Self::OpenAi,
        }
    }
}

fn resolve_embedder(
    path: Option<&std::path::Path>,
    backend: Option<EmbeddingsArg>,
) -> anyhow::Result<ctxq::core::vector::Embedder> {
    let root = ctxq::cli::config::ProjectPaths::discover(path)
        .map(|p| p.root_dir)
        .or_else(|_| std::env::current_dir())?;
    let settings = ctxq::core::settings::Settings::load(&root)?;
    ctxq::core::vector::Embedder::from_settings(&settings, backend.map(Into::into))
}

fn filters_of(
    tags: Vec<String>,
    source_types: Vec<String>,
    since: Option<String>,
    path_prefixes: Vec<String>,
) -> SearchFilters {
    SearchFilters {
        tags,
        source_types,
        since,
        path_prefixes,
    }
}

fn main_pdf_child() -> Option<anyhow::Result<()>> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).map(|a| a == ctxq::core::convert::PDF_CHILD_ARG) != Some(true) {
        return None;
    }
    #[cfg(feature = "pdf")]
    {
        let (Some(i), Some(o)) = (args.get(2), args.get(3)) else {
            return Some(Err(anyhow::anyhow!("usage: __pdf-text <in> <out>")));
        };
        Some(ctxq::core::convert::pdf_text_child(i.as_ref(), o.as_ref()))
    }
    #[cfg(not(feature = "pdf"))]
    Some(Err(anyhow::anyhow!("built without the 'pdf' feature")))
}

fn main() -> anyhow::Result<()> {
    if let Some(res) = main_pdf_child() {
        return res;
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Index { force_home } => run_index(
            cli.path.as_deref(),
            resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
            force_home,
        )?,
        Commands::Sync { force_home } => run_sync(
            cli.path.as_deref(),
            resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
            force_home,
        )?,
        Commands::Doctor { workspace, all } => run_doctor(cli.path.as_deref(), &workspace, all)?,
        Commands::Daemon { stop: true, .. } => run_daemon_stop(cli.path.as_deref())?,
        Commands::Daemon { http, .. } => {
            ctxq::cli::daemon::run_daemon(
                cli.path.as_deref(),
                resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
                http,
            )?;
        }
        Commands::Q {
            query,
            k,
            full,
            json,
            stop,
            no_daemon,
            tags,
            source_types,
            since,
            path_prefixes,
        } => {
            let filters = filters_of(tags, source_types, since, path_prefixes);
            run_q(
                QRequest {
                    target: cli.path.as_deref(),
                    query: query.as_deref(),
                    k,
                    full,
                    json_out: json,
                    stop,
                    no_daemon,
                    filters: &filters,
                },
                || resolve_embedder(cli.path.as_deref(), cli.embeddings),
            )?;
        }
        Commands::Inventory {
            kind,
            filter,
            limit,
        } => {
            let req = serde_json::json!({ "op": "inventory", "kind": kind, "filter": filter, "limit": limit });
            run_agent_op(cli.path.as_deref(), req, || {
                resolve_embedder(cli.path.as_deref(), cli.embeddings)
            })?;
        }
        Commands::Map {
            prefix,
            depth,
            limit,
        } => {
            let req = serde_json::json!({ "op": "map", "prefix": prefix, "depth": depth, "limit": limit });
            run_agent_op(cli.path.as_deref(), req, || {
                resolve_embedder(cli.path.as_deref(), cli.embeddings)
            })?;
        }
        Commands::Status => {
            run_agent_op(
                cli.path.as_deref(),
                serde_json::json!({ "op": "status" }),
                || resolve_embedder(cli.path.as_deref(), cli.embeddings),
            )?;
        }
        Commands::Annotate {
            path,
            summary,
            tags,
            author,
        } => {
            let req = serde_json::json!({ "op": "annotate", "path": path, "summary": summary, "tags": tags, "author": author });
            run_agent_op(cli.path.as_deref(), req, || {
                resolve_embedder(cli.path.as_deref(), cli.embeddings)
            })?;
        }
        Commands::Store { action } => run_store(
            &action,
            cli.path.as_deref(),
            resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
        )?,
        Commands::Convert { file } => {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            let title = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = std::fs::read(&file)?;
            print!("{}", ctxq::core::convert::to_markdown(ext, &bytes, &title)?);
        }
        Commands::Ingest {
            intake,
            context,
            force,
            json,
        } => run_ingest(&intake, &context, force, json)?,
        Commands::ExportMap {
            context,
            profile,
            intake,
            force,
            agents_template,
            json,
        } => run_export_map(
            &context,
            &profile,
            &intake,
            force,
            agents_template.as_deref(),
            json,
        )?,
        Commands::Scan { only, sync } => {
            run_scan(cli.path.as_deref(), &only)?;
            if sync {
                run_sync(
                    cli.path.as_deref(),
                    resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
                    false,
                )?;
            }
        }
        Commands::ImportVectors { chunks, vectors } => {
            run_import_vectors(
                &chunks,
                &vectors,
                cli.path.as_deref(),
                resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
            )?;
        }
        Commands::Search {
            query,
            limit,
            full,
            no_daemon,
            tags,
            source_types,
            since,
            path_prefixes,
            json,
        } => {
            let filters = filters_of(tags, source_types, since, path_prefixes);
            run_search(
                SearchRequest {
                    target: cli.path.as_deref(),
                    query: &query,
                    limit,
                    full,
                    json_out: json,
                    no_daemon,
                    filters: &filters,
                },
                || resolve_embedder(cli.path.as_deref(), cli.embeddings),
            )?;
        }
        Commands::Mcp { workspace } => run_serve(
            cli.path.as_deref(),
            &workspace,
            resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
        )?,
        Commands::Setup {
            client,
            uninstall,
            command,
            rules,
            skill,
            hooks,
            in_repo,
            force_home,
        } => {
            let embedder = if uninstall {
                ctxq::core::vector::Embedder::local()
            } else {
                resolve_embedder(cli.path.as_deref(), cli.embeddings)?
            };
            run_setup(SetupOptions {
                target: cli.path.as_deref(),
                clients: &client,
                uninstall,
                command: command.as_deref(),
                rules,
                skill,
                hooks,
                in_repo,
                force_home,
                embedder,
            })?;
        }
        Commands::Changes {
            base,
            head,
            depth,
            json,
            paths,
        } => run_changes(
            cli.path.as_deref(),
            base.as_deref(),
            head.as_deref(),
            depth,
            &paths,
            json,
        )?,
        Commands::Explore {
            symbol,
            depth,
            max_tokens,
            json,
        } => run_explore(cli.path.as_deref(), &symbol, depth, max_tokens, json)?,
        Commands::Context {
            question,
            scope,
            max_tokens,
            page,
            json,
        } => run_context(
            cli.path.as_deref(),
            &question,
            &scope,
            max_tokens,
            page,
            resolve_embedder(cli.path.as_deref(), cli.embeddings)?,
            json,
        )?,
        Commands::Stats {
            workspace,
            json,
            purge,
        } => run_stats(workspace.as_deref(), json, purge)?,
    }
    Ok(())
}
