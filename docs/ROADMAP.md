# Roadmap — documentum v3

## Relevance gate — `bench/eval_docs.py`

Any change to chunking, tokenisation, fusion, embeddings or stores must be
re-run against the golden set (private, `eval/README.md`).

| Version | Embeddings | MRR (n=100) | r@1 | r@5 | miss |
|---|---|---:|---:|---:|---:|
| v3.0.0 (2026-10-23) | qwen3-embedding 0.6b, "home server" template | **0.795** [0.739–0.848] | 0.71 | 0.92 | 8 |
| v3.0.0 | qwen3-embedding 0.6b, generic template | 0.754 | 0.67 | 0.88 | 12 |
| v3.0.0 | `local` 128d (offline) | ≈ 0.51 | | | |

Threshold: MRR ≥ 0.793.

## v3.0.0 — rewrite (2026-10-23)

- Document core: H2/H3 chunking with breadcrumbs, BM25 + vectors, weighted
  score fusion for identifiers, incremental sync, filters, stores (qdrant,
  peer), daemon, inventories, ingest/export-map.
- In-house code graph ([v3/CODEGRAPH.md](v3/CODEGRAPH.md)): generic lexer plus
  recognizers for Rust, TS/JS, Python, Go, Java, Kotlin, C#. Symbols,
  callers/callees, paths, diff impact, doc ↔ code links. No third-party
  parser.
- Stdio MCP server (`search`, `context`, `code_*`, `inventory`, `map`, `status`,
  `annotate`) and `setup` for 11 clients.
- Functional specification: [v3/SPEC.md](v3/SPEC.md).

## To do

- Align the chunking key (`md-h2h3-v1`) with existing indexes, or go through
  `import-vectors`, before switching over any real index (otherwise a full
  re-embedding).
- Code graph: calls inside string interpolations, C# indexers, Kotlin primary
  constructors, `Program.cs` with top-level statements.
- Ideas: file watcher (inotify/FSEvents) in the daemon; OS conversion fallback
  for .doc/.rtf/.odt; OS metadata (Finder tags) as filters.
