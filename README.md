# documentum

**Your docs and code, sorted and indexed _for your AI agent_ — one binary.**
It stops digging through folders: it asks, and gets back small, **sourced
passages** over MCP, a resident daemon or the CLI.

- **No lost context**: the few passages it needs, with their source, instead of
  whole files or guessed paths.
- **Always-current docs**: the index re-syncs incrementally as files change,
  and `status` tells the agent how fresh it is.

![documentum: a pile of documents transformed into a structured index](https://documentum.kerliane.eu/hero-flat.webp)

- **Relevant**: hybrid search — dense vectors **and** a global BM25, fused by
  score, with identifier-aware weighting. Exact identifiers (a port, a
  container, a domain) are found as reliably as paraphrased questions.
- **Local first**: the index is a single SQLite file. Works offline with the
  built-in embedder; plug Ollama or any OpenAI-compatible server for a real
  model. Vectors can live in Qdrant, and other bases can be federated.
- **Agent oriented**: stable JSON, bounded outputs, structured inventories,
  workspace maps, annotations that agents leave for each other, and a
  `status` they can trust.
- **Safe by default**: files carrying secrets (keys, tokens) are quarantined
  before indexing — never chunked, never embedded, never served.

v3 is a ground-up rewrite: a retrieval core plus a code
graph (symbols, callers, change impact, doc ↔ code links —
[docs/v3/CODEGRAPH.md](docs/v3/CODEGRAPH.md)).

## How it indexes

- **Documents** — Word, PowerPoint (with notes), Excel and PDF are converted to
  markdown in-process, then cut by H2/H3 headings; each chunk keeps its
  breadcrumb. Tables and inventories are indexed one row per chunk. Sync is
  incremental (hash + mtime).
- **Code** — a generic lexer, tuned per language, feeds a scope tree and
  per-language declaration recognizers: symbols, calls resolved through
  imports, doc comments, doc ↔ code links. Rust, TS/JS, Python, Go, Java,
  Kotlin, C#.
- **Ranking** — dense cosine + a global BM25, fused by score:
  `cosine + α · bm25 / bm25_max`, with α = 0.5 for identifier-like queries and
  0.2 for natural language.

## Measured

A 100-question golden set (exact-identifier and natural-language questions from
a real private workspace, kept out of the repo) gates every change.

| Configuration | MRR | hit@1 | hit@5 | missed |
|---|---:|---:|---:|---:|
| Hybrid, qwen3-embedding 0.6b, tuned prompt template | **0.795** [0.739–0.848] | 71 % | 92 % | 8 |
| Same, generic prompt template | 0.754 | 67 % | 88 % | 12 |
| Built-in embedder, fully offline (128-d) | ≈ 0.51 | | | |

Hybrid score fusion beat reciprocal rank fusion on that set. Details and
history: [docs/ROADMAP.md](docs/ROADMAP.md).

## Install

One binary, no runtime dependency. Prebuilt releases (Linux x86_64/arm64
static, macOS Intel/Apple silicon) are built by GitHub Actions and verified
against the release's `SHA256SUMS` by the installer:

```bash
curl -fsSL https://documentum.kerliane.eu/install.sh | bash      # Linux, macOS, WSL → ~/.local/bin
```

Native Windows build: not yet (use WSL meanwhile).

`DOCUMENTUM_VERSION=v3.0.1` pins a release, `DOCUMENTUM_BINDIR` changes the
destination. Project page: <https://documentum.kerliane.eu>.

From source:

```bash
cargo install --git https://github.com/kerbart/documentum.git --locked
# or: cargo build --release            # target/release/documentum
# optional embedded ONNX model:  cargo build --release --features onnx-embeddings
```

Cutting a release: bump `version` in `Cargo.toml`, commit, then
`scripts/release.sh vX.Y.Z` (tags and pushes; `.github/workflows/release.yml`
does the rest). `ci.yml` runs `cargo test --release` on every push.

## Quick start

```bash
cd my-workspace
documentum setup                      # 1. index + register documentum in your agents
documentum search "how do I deploy the api?"   # 2. ask it yourself
```

That is all. **Your agents** (Claude Code, Codex, Cursor, VS Code, Zed…) now
list documentum among their tools — restart them once. You never start the
MCP server yourself: the editor launches `documentum mcp` in the background
and talks to it over stdin/stdout.

`search` keeps the index warm in a small background daemon it starts on first
use (milliseconds per query afterwards); `--no-daemon` searches in-process.
Filters: `--tag legal --type pdf --since 2026-01-01 --path docs/`, `--json` for
scripts. `documentum sync` refreshes the index after big changes (the daemon
also re-syncs periodically).

For real semantic search, put a `documentum.toml` at the workspace root (or
point `$DOCUMENTUM_CONFIG` at one) and pick an embedding server:

```toml
# A local or LAN server — no credentials
[embed]
backend = "ollama"
urls = ["http://gpu-box:11434", "http://localhost:11434"]   # tried in order
model = "qwen3-embedding:0.6b"
```

```toml
# An API with a key — OpenAI or any compatible server (LM Studio, vLLM, TEI…)
[embed]
backend = "openai"
urls = ["https://api.openai.com"]
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"        # the NAME of a variable, not the key
```

```bash
export OPENAI_API_KEY="sk-..."        # shell profile or secret manager
```

**The key never goes in the file**: `documentum.toml` can be committed, the key
stays in your environment and is sent as `Authorization: Bearer` to the
embedding server only. Same pattern for Qdrant, federated peers
(`api_key_env`) and the daemon's HTTP API (`http_token_env`).

## What gets indexed

| Kind | How |
|---|---|
| Markdown | sections by H2/H3 with breadcrumbs; front matter (`title`, `summary`, `tags`, `date`, `source_type`) becomes metadata and filters |
| Office / PDF | `.docx`, `.pptx` (with speaker notes), `.xlsx` (one row = one passage), `.pdf` → markdown, in-process |
| Inventories | `[[scan.source]]`: any command's output, or built-in `docker`, `systemd`, `ports`, `projects`, `scripts` collectors; tables indexed row by row |
| Code | Rust, TypeScript/JavaScript, Python, Go, Java, Kotlin, C#: symbols, signatures, doc comments, callers/callees, bodies, change impact, doc ↔ code links (`[code] enabled`) |

Discovery honours `roots` (in priority order), `.documentumignore`,
`.gitignore`, size limits, and skips third-party clones (`own_remotes`).

## Agent surface

| MCP tool | Use it for |
|---|---|
| `inventory(kind, filter)` | exact rows: "which port / container / domain" |
| `search(query, tags, source_types, since, path_prefixes, max_chars)` | ranked passages with metadata |
| `context(query, scope, max_tokens, page)` | docs and code together, within a token budget |
| `map(prefix, depth)` | one-line summary per document — read the map, open only what you need |
| `annotate(path, summary, tags)` | leave what you learned for the next agent |
| `status()` | freshness, store, sources, quarantined and lost files |
| `code_symbol(name)` | signature, doc and body of a symbol |
| `code_callers` / `code_callees(name, depth)` | who calls it, what it calls |
| `code_path(from, to)` | shortest call chain between two symbols |
| `code_impact(diff \| base, head)` | changed symbols → transitive callers → docs that mention them |
| `code_sync(paths)` | re-extract changed files now |

The same operations exist as CLI commands (`search`, `context`, `explore`,
`changes`, `inventory`, `map`, `status`, `annotate`) and, for docs, on the
daemon's HTTP API.

## Daemon

Optional — `search` starts it on demand. Run it yourself to expose the HTTP
API (federation, other tools) or to keep it up permanently:

```bash
documentum daemon --http 127.0.0.1:7700
documentum daemon --stop
```

A warm query costs a few milliseconds plus the query embedding (cached,
persistently). The daemon can re-sync and re-scan periodically (`[daemon]`).

## Vector stores and federation

```toml
[store]
backend = "qdrant"                    # default "local" (SQLite, exact)
url = "http://localhost:6333"
collection = "my-docs"

[[store.federate]]                    # read-only bases queried with every search
backend = "peer"                      # another documentum daemon (POST /search)
url = "http://other-host:7700"
name = "team-docs"
weight = 0.8
```

`documentum store push` uploads existing vectors without re-embedding.

## Configuration

Precedence: command-line flags > `DOCUMENTUM_*` environment > `documentum.toml`
(or `.documentum.toml`, or `$DOCUMENTUM_CONFIG`) > defaults. Every key is
documented in [documentum.example.toml](documentum.example.toml).

## Measuring

`bench/eval_docs.py` replays a golden question set (recall@k, MRR, bootstrap
confidence interval) against one or several engines; nothing changes the
ranking without passing it.

## License

MIT — see [LICENSE](LICENSE).
