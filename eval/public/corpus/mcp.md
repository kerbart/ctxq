# MCP tools

The MCP server name is `ctxq`. Agents should start with:

- `inventory` — exact rows (port, container, domain), no vector search
- `search` — ranked passages with source path and line range
- `status` — freshness, quarantined files, stores
- `context` — docs and code together, paged by a token budget

Code graph: `code_symbol`, `code_callers`, `code_callees`, `code_path`,
`code_impact`. After edits, `code_sync`. Leave notes with `annotate`.

The editor launches `ctxq mcp` over stdin/stdout. You never start it yourself.
