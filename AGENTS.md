# AGENTS.md — using ctxq from an agent

This workspace is indexed by `ctxq` (MCP server name `ctxq`). Ask it
before grepping or reading files: answers are short, sourced (`path:lines`) and
cost far fewer tokens than a read loop.

## Which tool

| You want | Call |
|---|---|
| An exact fact (port, container, domain, unit) | `inventory(kind, filter)` |
| Passages about a question | `search(query)` — filters: `tags`, `source_types`, `since`, `path_prefixes` |
| Docs and code together, within a budget | `context(query, scope="auto"\|"docs"\|"code", max_tokens, page)` |
| A symbol's signature, doc and body | `code_symbol(name)` — page a long body with `start_line`/`end_line` |
| Who calls it / what it calls | `code_callers(name, depth)` / `code_callees(name, depth)` |
| How A reaches B | `code_path(from, to)` |
| What a change breaks | `code_impact(diff)` or `code_impact(base, head)` |
| What a folder contains | `map(prefix, depth)` |
| Whether the index is fresh | `status()` |

Every tool accepts `workspace` when the server serves several.

## Habits

- Read the returned body instead of reopening the file; open the file only for
  lines outside the returned range.
- After editing source files, call `code_sync(paths)` so later queries see the
  new symbols.
- Ambiguous names come back as a list of candidates: retry with the qualified
  name (`Type.method`).
- Two empty answers in a row (after rephrasing once): fall back to `rg` or a
  bounded read. Literal strings (log lines, error codes) are a grep job from the
  start.
- Learned something worth keeping? `annotate(path, summary, tags)` — the next
  agent will see it in `map` and `search`.
