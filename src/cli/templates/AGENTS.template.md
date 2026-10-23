# Agent notes

This repository is indexed by `documentum`.

Prefer the MCP tools before walking the tree by hand:

- `context` — docs and code together, paged by a token budget
- `search` — hybrid search over the documents
- `code_symbol`, `code_callers`, `code_callees`, `code_path` — one symbol and its neighbourhood
- `code_impact` — callers and documents touched by a diff
- `inventory`, `map`, `status`, `annotate` — inventories, the workspace map, freshness, notes

Pass `workspace` when the server was started with several `--workspace` entries.
The first workspace is the default.
