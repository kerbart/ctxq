# CLAUDE.md — documentum

Instructions for agents working **on this repository** (documentum v3, Rust).
Retrieval rules for agents *using* documentum: [AGENTS.md](AGENTS.md).

## Build & test

- `cargo build --release` — single binary `target/release/documentum`.
- `cargo test --release` — unit + integration (no network).
- Release builds use thin LTO: a full test rebuild takes ~10 min on a small box.

## Relevance gate (never skip)

Any change touching chunking, tokenisation, fusion, embeddings or stores must
keep the golden set: `bench/eval_docs.py` (100 questions, MRR + bootstrap CI).
Report the MRR before/after and the per-question divergences; a change that
"looks better" without the gate is not done.

## Invariants

- Nothing machine-specific in the code: paths, models, services, prefixes go
  to `documentum.toml` (documented in `documentum.example.toml`, which a test
  parses).
- Own locations only: cache `~/.cache/documentum/`, in-repo `.documentum/`,
  env `DOCUMENTUM_*`, MCP server name `documentum`.
- The code graph (`src/code/`) is ours: no tree-sitter, no third-party parser
  crate. New language = a `LangSpec` + a recognizer (docs/v3/CODEGRAPH.md).
  Bump `EXTRACTOR_VERSION` whenever extraction output changes.
- stdout belongs to MCP JSON-RPC in `mcp`: logs to stderr, chatty libraries
  in a child process (see `convert::pdf`).
- Remote embedding is never implicit (no auto-index with a remote embedder)
  and bounded (`[embed] concurrency`, `batch`): the server is often shared.
- Secrets: files are scanned before chunking, embedding and serving bodies.

## Map

- `src/core/hybrid.rs` — doc engine: sync, fusion search, filters, stores, federation.
- `src/core/{mdchunk,lexical,embed_http,settings,collect,secrets,convert,docmeta,agent}.rs`
- `src/core/ingest.rs` — intake mechanics (`ingest`, `export-map`): intake → staging → context map.
- `src/core/sources/` — inventory sources (command + built-ins).
- `src/core/store/` — vector stores (qdrant, peer).
- `src/cli/daemon.rs` — daemon (used by `search`), HTTP API.
- `src/code/` — code graph: `lex`, `scope`, `lang/*`, `extract`, `store`, `sync`, `query`.
- `src/mcp/` — MCP server (JSON-RPC, tools, budgets); `src/setup/` — client registration.
- `src/cli/` — commands; `docs/v3/SPEC.md` — functional spec of v3.
- `docs/ROADMAP.md` — phases, gates, measurements.
