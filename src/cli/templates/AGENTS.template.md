# Agent notes

This repository is indexed by `ctxq`.

Use the `ctxq` MCP tools. If they are missing or blocked by policy, run
`ctxq search`, `ctxq context`, `ctxq explore` and `ctxq changes` instead.
Codex cannot: its `bwrap` sandbox cannot launch the binary, so only MCP
works there.

1. `inventory` — exact rows (port, container, domain, unit)
2. `search` — ranked passages with source lines
3. `status` — is the index fresh?
4. `context` — docs and code together, inside a token budget
5. `code_symbol` then `code_callers` / `code_callees` / `code_path` / `code_impact`
6. `code_sync` after you edit source; `annotate` to leave a note for the next agent

If the tools are missing, restart the agent and check `/mcp`.
Pass `workspace` when the server was started with several `--workspace` entries.
The first workspace is the default.
