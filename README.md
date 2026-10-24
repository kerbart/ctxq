# ctxq

**Ask the index, get the line.** Your docs, live inventories and code,
indexed _for your AI agent_ — one binary. It stops digging through folders:
it asks, and gets back small, **sourced passages** over MCP, a resident
daemon or the CLI.

- **No lost context**: the few passages it needs, with their source, instead of
  whole files or guessed paths.
- **Always-current docs**: the daemon re-syncs docs every 60s and inventories
  every 5 min; `status` tells the agent how fresh the index is.

![ctxq: a pile of documents transformed into a structured index](https://ctxq.dev/hero-flat.webp)

- **Relevant**: a global BM25 with identifier-aware weighting, plus dense
  cosine when you plug a real embedder. Offline, BM25 leads (the built-in
  hash is a weak rerank, not a neural model). Exact identifiers — a port, a
  container, a domain — hit either way.
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
  `cosine + α · bm25 / bm25_max`. Identifier-like queries get a higher α.
  With the built-in hash embedder, α is raised so BM25 leads.

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

One binary, no runtime dependency. Source and releases live at
[`github.com/kerbart/ctxq`](https://github.com/kerbart/ctxq). The one-liner
downloads a SHA-256-verified archive from GitHub Releases (ctxq.dev mirrors
them), installs it to `~/.local/bin`, and puts `ctxq` on **this** shell's PATH.
Linux x86_64 is musl-static (Alpine and glibc distros). Linux arm64 and macOS
ship from the same command.

```bash
curl -fsSL https://ctxq.dev/install.sh | sh && . "$HOME/.ctxq/env"
# same installer: https://raw.githubusercontent.com/kerbart/ctxq/main/install.sh
```

Then, from a project:

```bash
ctxq setup
ctxq search "how do I deploy the api?"
```

Windows: use WSL. `CTXQ_BINDIR` changes the destination. Project page:
<https://ctxq.dev>.

From source:

```bash
cargo install --git https://github.com/kerbart/ctxq.git --locked
# or: cargo build --release            # target/release/ctxq
# optional embedded ONNX model:  cargo build --release --features onnx-embeddings
```

Cutting a release: bump `version` in `Cargo.toml`, commit, then
`scripts/release.sh vX.Y.Z` (tags and pushes; `.github/workflows/release.yml`
does the rest). `ci.yml` runs `cargo test --release` on every push.

## Quick start

```bash
cd my-workspace
ctxq setup                      # 1. index + register ctxq in your agents
ctxq search "how do I deploy the api?"   # 2. ask it yourself
```

That is all. **Your agents** (Claude Code, Codex, Cursor, VS Code, Zed…) now
list ctxq among their tools — restart them once. You never start the
MCP server yourself: the editor launches `ctxq mcp` in the background
and talks to it over stdin/stdout.

## Where the index lives

One index **per workspace root** (the directory you ran `ctxq setup` in).
It sits outside the tree, at `~/.cache/ctxq/<name>-<hash>/docs.db` (or
`<root>/.ctxq/` if you passed `--in-repo`). `ctxq doctor` prints the path.

`ctxq setup --force-home` indexes **all of `$HOME`**. That is a different
index from a project inside it:

```
ctxq setup --force-home          # index A: ~/.cache/ctxq/<you>-…/docs.db
cd ~/engie && ctxq setup         # index B: ~/.cache/ctxq/engie-…/docs.db
```

Two indexes. They do not merge. Searching from `~/engie` uses B; searching
from `$HOME` uses A (which also contains the engie files as a subset).
Markdown, office, PDF and code are indexed; `.git`, archives, secrets and
gitignored files are skipped.

Codex is registered automatically when `~/.codex/` exists (`ctxq setup`
writes `.codex/config.toml` in the project and `~/.codex/config.toml`).
Restart Codex, then `/mcp`. Do not run `ctxq` inside the Codex shell: its
bwrap sandbox cannot launch it; the MCP tools are the path that works.

`search` keeps the index warm in a background daemon it starts on first use
(milliseconds per query afterwards; docs re-sync every 60s). `--no-daemon`
searches in-process.
Filters: `--tag legal --type pdf --since 2026-01-01 --path docs/`, `--json` for
scripts. `ctxq sync` refreshes the index after big changes (the daemon
also re-syncs periodically).

For real semantic search, put a `ctxq.toml` at the workspace root (or
point `$CTXQ_CONFIG` at one) and pick an embedding server:

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

**The key never goes in the file**: `ctxq.toml` can be committed, the key
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

Discovery honours `roots` (in priority order), `.ctxqignore`,
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
ctxq daemon --http 127.0.0.1:7700
ctxq daemon --stop
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
backend = "peer"                      # another ctxq daemon (POST /search)
url = "http://other-host:7700"
name = "team-docs"
weight = 0.8
```

`ctxq store push` uploads existing vectors without re-embedding.

## Configuration

Precedence: command-line flags > `CTXQ_*` environment > `ctxq.toml`
(or `.ctxq.toml`, or `$CTXQ_CONFIG`) > defaults. Every key is
documented in [ctxq.example.toml](ctxq.example.toml).

## Measuring

`bench/eval_docs.py` replays a golden question set (recall@k, MRR, bootstrap
confidence interval) against one or several engines; nothing changes the
ranking without passing it.

## License

MIT — see [LICENSE](LICENSE).
