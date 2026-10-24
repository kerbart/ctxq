---
name: ctxq-investigate
description: Co-retrieve documentation and the code graph with ctxq before editing.
---

# Investigation

1. Call `context` with the question. Scope is `auto`, `docs` or `code`. When the ledger says more entries exist, ask for the next `page`.
2. Call `search` for a passage in the documents.
3. Call `code_symbol` for one symbol: signature, doc comment, bounded body.
4. Call `code_callers` and `code_callees` for the neighbourhood. `code_path` connects two symbols.
5. After a diff, call `code_impact` (or the `changes` command) to list callers and the documents that mention the touched symbols.
6. Call `code_sync` after editing source files so the graph matches the tree.

Prefer these tools before a blind repository walk. Do not shell `ctxq`
(Codex sandbox cannot launch it); the MCP server is the path that works.
