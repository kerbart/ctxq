//! Configuration: `ctxq.toml` at the workspace root (or `$CTXQ_CONFIG`),
//! overridden by `CTXQ_*` environment variables, themselves overridden by
//! command-line flags.
//!
//! Nothing tied to a particular machine, model or service lives in the code.
//! Defaults target an ordinary offline install (local embedder, current
//! directory); everything else is declared.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_parsed<T: std::str::FromStr>(name: &str) -> Option<T> {
    env(name).and_then(|v| v.trim().parse().ok())
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmbedBackend {
    /// Hashed n-gram vectors computed in-process: no network, no download.
    #[default]
    Local,
    /// Embedded ONNX model (requires the `onnx-embeddings` build feature).
    Onnx,
    /// Ollama server(s), `POST /api/embed`.
    Ollama,
    /// OpenAI-compatible server(s), `POST /v1/embeddings` (OpenAI, LM Studio,
    /// llama.cpp server, vLLM, text-embeddings-inference, Infinity…).
    OpenAi,
}

impl std::str::FromStr for EmbedBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "local" => Self::Local,
            "onnx" => Self::Onnx,
            "ollama" => Self::Ollama,
            "openai" => Self::OpenAi,
            other => bail!("unknown embedding backend '{other}' (local, onnx, ollama, openai)"),
        })
    }
}

/// Task templates of a model: `{t}` = document text, `{q}` = query.
#[derive(Debug, Clone, Deserialize)]
pub struct Templates {
    #[serde(default = "passthrough_doc")]
    pub document: String,
    #[serde(default = "passthrough_query")]
    pub query: String,
}

fn passthrough_doc() -> String {
    "{t}".into()
}
fn passthrough_query() -> String {
    "{q}".into()
}

/// Public model families, recognised by substring of the model name.
///
/// Most embedding models are trained with mandatory, distinct prefixes for
/// documents and queries; omitting them degrades relevance silently.
const KNOWN_TEMPLATES: &[(&str, &str, &str)] = &[
    ("nomic", "search_document: {t}", "search_query: {q}"),
    (
        "qwen3",
        "{t}",
        "Instruct: Given a question, retrieve documentation passages that answer it\nQuery: {q}",
    ),
    ("e5", "passage: {t}", "query: {q}"),
    (
        "embeddinggemma",
        "title: none | text: {t}",
        "task: search result | query: {q}",
    ),
    ("arctic-embed", "{t}", "query: {q}"),
    ("bge", "{t}", "{q}"),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EmbedSettings {
    pub backend: EmbedBackend,
    /// Instances tried in order; the first one that answers becomes preferred.
    pub urls: Vec<String>,
    /// Empty = backend default (`qwen3-embedding:0.6b` for Ollama).
    pub model: String,
    /// NAME of the environment variable holding the API key, never the key:
    /// this file is meant to be versioned.
    pub api_key_env: Option<String>,
    /// Truncation (characters) before sending, for a deterministic index.
    pub max_chars: usize,
    /// Concurrent requests per process. Keep 1 when the server is shared with
    /// interactive queries: a rebuild must not starve them.
    pub concurrency: usize,
    /// Texts per request at most (bounds the wait of a query queued behind a batch).
    pub batch: usize,
    pub connect_timeout_secs: u64,
    pub read_timeout_secs: u64,
    /// Explicit templates, keyed by model-name substring; win over known families.
    pub templates: BTreeMap<String, Templates>,
}

impl Default for EmbedSettings {
    fn default() -> Self {
        Self {
            backend: EmbedBackend::Local,
            urls: Vec::new(),
            model: String::new(),
            api_key_env: None,
            max_chars: 6000,
            concurrency: 1,
            batch: 8,
            connect_timeout_secs: 2,
            read_timeout_secs: 300,
            templates: BTreeMap::new(),
        }
    }
}

impl EmbedSettings {
    pub fn templates_for(&self, model: &str) -> (String, String) {
        let lower = model.to_lowercase();
        if let Some((_, t)) = self
            .templates
            .iter()
            .find(|(key, _)| lower.contains(&key.to_lowercase()))
        {
            return (t.document.clone(), t.query.clone());
        }
        KNOWN_TEMPLATES
            .iter()
            .find(|(k, _, _)| lower.contains(k))
            .map(|(_, d, q)| (d.to_string(), q.to_string()))
            .unwrap_or_else(|| ("{t}".into(), "{q}".into()))
    }

    pub fn api_key(&self) -> Option<String> {
        self.api_key_env.as_deref().and_then(env)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Fusion {
    /// `cos + α · bm25 / bm25_max(query)` — measured best on ctxq's golden set.
    #[default]
    Score,
    /// Reciprocal Rank Fusion of FTS5 and vector ranks (inherited from ctxq).
    Rrf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SearchSettings {
    pub fusion: Fusion,
    /// Lexical weight for natural-language questions.
    pub alpha: f32,
    /// Lexical weight for queries carrying or asking for an exact identifier.
    pub alpha_id: f32,
    /// Dense candidate pool.
    pub pool: usize,
    /// Candidates added from the global BM25.
    pub lex_pool: usize,
    /// Results per file at most (0 = unlimited).
    pub max_per_source: usize,
    /// Identical texts at least this long are kept once.
    pub dedup_min_chars: usize,
    /// Words that announce an identifier lookup even when the query holds none
    /// (« what port does X use »). Replaces the built-in list when set.
    pub lookup_terms: Option<Vec<String>>,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            fusion: Fusion::Score,
            alpha: 0.2,
            alpha_id: 0.5,
            pool: 60,
            lex_pool: 40,
            max_per_source: 2,
            dedup_min_chars: 200,
            lookup_terms: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CollectSettings {
    /// Directories to index, relative to the workspace root (or absolute),
    /// in priority order: when two chunks are identical, the one from the
    /// earliest root survives deduplication.
    pub roots: Vec<String>,
    /// Restrict document discovery to these extensions (empty = every
    /// supported type: markdown, specs, contracts, configs, workflows…).
    pub extensions: Vec<String>,
    /// Text files larger than this are skipped (bytes, 0 = no limit).
    pub max_file_bytes: u64,
    /// Same for office documents and PDF, measured on the file itself (a deck
    /// is a zip of images around little text).
    pub max_document_bytes: u64,
    /// Extra directory names skipped at any depth (exact match).
    pub exclude_dirs: Vec<String>,
    /// Directory names skipped at any depth, case-insensitive (archives…).
    pub exclude_dir_names: Vec<String>,
    /// Top-level directories of the root that are not walked (they can still
    /// be listed in `roots`, e.g. skip `tool/` but index `tool/docs`).
    pub exclude_top: Vec<String>,
    /// File of globs (one per line, relative to the root, `#` comments).
    pub exclude_globs_file: Option<String>,
    /// Inline globs, same syntax, added to the file's.
    pub exclude_globs: Vec<String>,
    /// A sub-directory holding its own git repository whose remote URL matches
    /// none of these substrings is treated as a third-party clone and skipped.
    /// Empty = rule disabled: we do not guess what belongs to whom.
    pub own_remotes: Vec<String>,
    pub respect_gitignore: bool,
    /// Directories whose large markdown tables are also indexed row by row
    /// (inventories: otherwise a question on one row drowns in the table).
    /// The scan output directory is always included.
    pub table_row_dirs: Vec<String>,
}

impl Default for CollectSettings {
    fn default() -> Self {
        Self {
            roots: vec![".".into()],
            extensions: Vec::new(),
            max_file_bytes: 1024 * 1024,
            max_document_bytes: 50 * 1024 * 1024,
            exclude_dirs: Vec::new(),
            exclude_dir_names: ["_archive", "archive", "archives", "backup", "backups"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            exclude_top: Vec::new(),
            exclude_globs_file: Some(".ctxqignore".into()),
            exclude_globs: Vec::new(),
            own_remotes: Vec::new(),
            respect_gitignore: true,
            table_row_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceFormat {
    /// The command already prints markdown: inserted as is.
    Markdown,
    /// Raw output wrapped in a code block.
    #[default]
    Code,
}

/// One inventory source: its output becomes a dated markdown document.
///
/// `kind = "command"` runs anything declared; the other kinds are optional
/// built-in collectors for things common to most Linux hosts, so the single
/// binary is enough without helper scripts.
#[derive(Debug, Clone, Deserialize)]
pub struct SourceSettings {
    /// Output file name, without extension.
    pub name: String,
    pub title: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
    /// `kind = "command"`: argv.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub format: SourceFormat,
    /// Paragraph placed under the title.
    pub intro: Option<String>,
    /// Kind-specific options (`dirs`, `exclude`, `socket`…).
    #[serde(flatten)]
    pub options: toml::Table,
}

fn default_kind() -> String {
    "command".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ScanSettings {
    /// Where generated inventories are written, relative to the root.
    pub output_dir: String,
    /// Language of the generated prose (`en`, `fr`).
    pub lang: String,
    pub source: Vec<SourceSettings>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            output_dir: ".ctxq/generated".into(),
            lang: "en".into(),
            source: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecretsSettings {
    /// Scan every file before indexing; suspicious files are quarantined
    /// (never chunked, never embedded, listed by `doctor`).
    pub enabled: bool,
    /// Additional content regexes.
    pub patterns: Vec<String>,
    /// Globs (relative to the root) exempted from the scan — known false positives.
    pub allow: Vec<String>,
}

impl Default for SecretsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            patterns: Vec::new(),
            allow: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StoreBackend {
    /// Vectors in the index's SQLite file, scanned in memory (exact cosine).
    #[default]
    Local,
    /// A Qdrant collection over HTTP.
    Qdrant,
    /// Another ctxq instance (its daemon's HTTP API); federation only.
    Peer,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StoreMode {
    /// `sync` writes our vectors there.
    #[default]
    Owned,
    /// Read-only: someone else's base, queried as is.
    Attached,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StoreSettings {
    pub backend: StoreBackend,
    pub mode: StoreMode,
    pub url: String,
    /// Collection (Qdrant). Empty = `ctxq`.
    pub collection: String,
    /// NAME of the environment variable holding the API key.
    pub api_key_env: Option<String>,
    /// Exact (brute force) search instead of the store's ANN index.
    pub exact: bool,
    /// Attached stores: payload fields carrying the text, source and title.
    pub text_field: String,
    pub source_field: String,
    pub title_field: String,
    /// Federated stores: multiplier applied to their (purely dense) scores.
    pub weight: f32,
    /// Label shown in federated results (default: the URL).
    pub name: Option<String>,
    pub timeout_secs: u64,
    /// Additional read-only stores queried with every search.
    pub federate: Vec<StoreSettings>,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            backend: StoreBackend::Local,
            mode: StoreMode::Owned,
            url: String::new(),
            collection: String::new(),
            api_key_env: None,
            exact: false,
            text_field: "text".into(),
            source_field: "source".into(),
            title_field: "title".into(),
            weight: 1.0,
            name: None,
            timeout_secs: 30,
            federate: Vec::new(),
        }
    }
}

impl StoreSettings {
    pub fn api_key(&self) -> Option<String> {
        self.api_key_env.as_deref().and_then(env)
    }

    pub fn collection_name(&self) -> String {
        if self.collection.is_empty() {
            "ctxq".into()
        } else {
            self.collection.clone()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Unix socket (default: `daemon.sock` in the workspace cache directory).
    pub socket: Option<String>,
    /// Optional HTTP API (`127.0.0.1:7700`): `POST /search`, `GET /status`… —
    /// what a federated peer calls. Off by default.
    pub http: Option<String>,
    /// NAME of the environment variable holding the bearer token required on HTTP.
    pub http_token_env: Option<String>,
    /// Incremental re-sync period (0 = never; the index changes only on `sync`).
    pub sync_interval_secs: u64,
    /// Inventory re-scan period (0 = never), followed by a sync.
    pub scan_interval_secs: u64,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            socket: None,
            http: None,
            http_token_env: None,
            sync_interval_secs: 60,
            scan_interval_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodeSettings {
    /// Build the code graph (symbols, callers, bodies). Off for a pure
    /// documentation workspace — e.g. a whole home directory whose code
    /// repositories are indexed as workspaces of their own.
    pub enabled: bool,
}

impl Default for CodeSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub embed: EmbedSettings,
    pub search: SearchSettings,
    pub collect: CollectSettings,
    pub scan: ScanSettings,
    pub secrets: SecretsSettings,
    pub store: StoreSettings,
    pub daemon: DaemonSettings,
    pub code: CodeSettings,
    /// File the settings came from, if any.
    #[serde(skip)]
    pub source_file: Option<PathBuf>,
}

impl Settings {
    /// `$CTXQ_CONFIG`, else `<root>/ctxq.toml`, else
    /// `<root>/.ctxq.toml`, else defaults; then environment overrides.
    pub fn load(root: &Path) -> Result<Self> {
        let candidates: Vec<PathBuf> = match env("CTXQ_CONFIG") {
            Some(p) => vec![PathBuf::from(p)],
            None => vec![root.join("ctxq.toml"), root.join(".ctxq.toml")],
        };
        let mut settings = Self::default();
        if let Some(path) = candidates.into_iter().find(|p| p.is_file()) {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            settings =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            settings.source_file = Some(path);
        } else if let Some(p) = env("CTXQ_CONFIG") {
            bail!("CTXQ_CONFIG points to a missing file: {p}");
        }
        settings.apply_env()?;
        Ok(settings)
    }

    /// Environment wins over the file: handy for benches and containers.
    fn apply_env(&mut self) -> Result<()> {
        // Inherited ctxq variable kept as a fallback.
        if let Some(v) = env("CTXQ_EMBEDDINGS") {
            self.embed.backend = v.parse()?;
        }
        if let Some(v) = env("CTXQ_EMBED_URLS") {
            self.embed.urls = list(&v)
                .into_iter()
                .map(|u| u.trim_end_matches('/').to_string())
                .collect();
        }
        if let Some(v) = env("CTXQ_EMBED_MODEL") {
            self.embed.model = v;
        }
        if let Some(v) = env("CTXQ_EMBED_API_KEY_ENV") {
            self.embed.api_key_env = Some(v);
        }
        if let Some(v) = env_parsed("CTXQ_EMBED_MAX_CHARS") {
            self.embed.max_chars = v;
        }
        if let Some(v) = env_parsed::<usize>("CTXQ_EMBED_CONCURRENCY") {
            self.embed.concurrency = v.max(1);
        }
        if let Some(v) = env_parsed::<usize>("CTXQ_EMBED_BATCH") {
            self.embed.batch = v.max(1);
        }
        if let Some(v) = env("CTXQ_FUSION") {
            self.search.fusion = if v.eq_ignore_ascii_case("rrf") {
                Fusion::Rrf
            } else {
                Fusion::Score
            };
        }
        if let Some(v) = env_parsed("CTXQ_ALPHA") {
            self.search.alpha = v;
        }
        if let Some(v) = env_parsed("CTXQ_ALPHA_ID") {
            self.search.alpha_id = v;
        }
        if let Some(v) = env_parsed("CTXQ_POOL") {
            self.search.pool = v;
        }
        if let Some(v) = env_parsed("CTXQ_LEX_POOL") {
            self.search.lex_pool = v;
        }
        if let Some(v) = env_parsed("CTXQ_MAX_PER_SOURCE") {
            self.search.max_per_source = v;
        }
        if let Some(v) = env("CTXQ_STORE") {
            self.store.backend = match v.to_ascii_lowercase().as_str() {
                "local" => StoreBackend::Local,
                "qdrant" => StoreBackend::Qdrant,
                other => bail!("CTXQ_STORE: unknown store '{other}' (local, qdrant)"),
            };
        }
        if let Some(v) = env("CTXQ_STORE_URL") {
            self.store.url = v;
        }
        if let Some(v) = env("CTXQ_STORE_COLLECTION") {
            self.store.collection = v;
        }
        if let Some(v) = env("CTXQ_ROOTS") {
            self.collect.roots = list(&v);
        }
        Ok(())
    }

    /// Effective model name for remote backends.
    pub fn embed_model(&self) -> String {
        if !self.embed.model.is_empty() {
            return self.embed.model.clone();
        }
        match self.embed.backend {
            EmbedBackend::OpenAi => "text-embedding-3-small".into(),
            _ => "qwen3-embedding:0.6b".into(),
        }
    }

    /// Effective server list for remote backends.
    pub fn embed_urls(&self) -> Vec<String> {
        if !self.embed.urls.is_empty() {
            return self.embed.urls.clone();
        }
        vec![match self.embed.backend {
            EmbedBackend::OpenAi => "http://localhost:8080".into(),
            _ => "http://localhost:11434".into(),
        }]
    }

    /// Directories (relative to the root) whose tables are chunked row by row.
    pub fn table_row_dirs(&self) -> Vec<String> {
        let mut dirs = self.collect.table_row_dirs.clone();
        dirs.push(self.scan.output_dir.clone());
        dirs.iter()
            .map(|d| d.trim_start_matches("./").trim_end_matches('/').to_string())
            .filter(|d| !d.is_empty() && d != ".")
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_example_parses_and_matches_defaults() {
        let s: Settings = toml::from_str(include_str!("../../ctxq.example.toml")).unwrap();
        let d = Settings::default();
        assert_eq!(s.embed.backend, d.embed.backend);
        assert_eq!(s.search.alpha, d.search.alpha);
        assert_eq!(s.collect.max_document_bytes, d.collect.max_document_bytes);
        assert_eq!(s.collect.exclude_dir_names, d.collect.exclude_dir_names);
        assert_eq!(s.scan.output_dir, d.scan.output_dir);
        assert_eq!(s.daemon.sync_interval_secs, d.daemon.sync_interval_secs);
        assert_eq!(s.daemon.scan_interval_secs, d.daemon.scan_interval_secs);
        assert!(s.scan.source.is_empty() && s.store.federate.is_empty());
    }

    #[test]
    fn defaults_are_offline_and_generic() {
        let s = Settings::default();
        assert_eq!(s.embed.backend, EmbedBackend::Local);
        assert_eq!(s.collect.roots, ["."]);
        assert!(s.secrets.enabled);
        assert_eq!(s.table_row_dirs(), [".ctxq/generated"]);
        assert_eq!(s.daemon.sync_interval_secs, 60);
        assert_eq!(s.daemon.scan_interval_secs, 300);
    }

    #[test]
    fn parses_sources_with_kind_options() {
        let s: Settings = toml::from_str(
            r#"
            [embed]
            backend = "ollama"
            model = "nomic-embed-text"
            [embed.templates.nomic]
            document = "D: {t}"
            query = "Q: {q}"
            [[scan.source]]
            name = "containers"
            kind = "docker"
            socket = "/run/docker.sock"
            [[scan.source]]
            name = "disk"
            command = ["df", "-h"]
            "#,
        )
        .unwrap();
        assert_eq!(s.embed.backend, EmbedBackend::Ollama);
        assert_eq!(s.embed.templates_for("nomic-embed-text").1, "Q: {q}");
        assert_eq!(
            s.scan.source[0].options["socket"].as_str(),
            Some("/run/docker.sock")
        );
        assert_eq!(s.scan.source[1].kind, "command");
        assert_eq!(s.scan.source[1].format, SourceFormat::Code);
    }
}
