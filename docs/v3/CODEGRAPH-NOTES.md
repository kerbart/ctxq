# Report: code graph storage & queries (`src/code/`)

Implements the "Storage" and "Queries" sections of `docs/v3/CODEGRAPH.md`. The
contract in `src/code/model.rs` was not modified; `extract.rs` remains the
other team's stub (all tests build `FileOutline`s by hand).

## Delivered files

- `src/code/store.rs`: `CodeStore`, SQLite schema, `upsert_file`/`remove_file`,
  `resolve_calls`.
- `src/code/sync.rs`: `sync_tree`, `IgnoreRules`, `SyncReport`.
- `src/code/query.rs`: `find`, `body`, `callers`/`callees`, `path`, `impact`,
  `doc_links`/`DocMentions`, types `SymbolHit`/`CallEdge`/`CallGraph`/
  `AmbiguousCall`/`ImpactReport`/`DocRef`.
- `src/code/mod.rs`: added `pub mod sync;`.

18 unit tests (`cargo test --lib code::`); `cargo clippy --lib` and `cargo fmt`
are clean on these files (the remaining clippy warnings in the crate are in
pre-existing files out of scope: `agent.rs`, `convert.rs`, `hybrid.rs`,
`ingest.rs`, `settings.rs`).

## Public API

```rust
pub struct FileMeta { pub path, lang, mtime, size, sha256 }
pub struct CodeStore;
impl CodeStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self>;
    pub fn in_memory() -> Result<Self>;
    pub fn from_connection(conn: Connection) -> Result<Self>; // idempotent schema
    pub fn conn(&self) -> &Connection;
    pub fn extractor_version(&self) -> Result<Option<u32>>;
    pub fn set_extractor_version(&self, version: u32) -> Result<()>;
    pub fn file_meta(&self, path: &str) -> Result<Option<FileMeta>>;
    pub fn all_file_paths(&self) -> Result<Vec<String>>;
    pub fn touch_file(&self, path: &str, mtime: i64, size: i64) -> Result<()>;
    pub fn upsert_file(&mut self, path: &str, lang: Lang, mtime: i64, size: i64,
                        sha256: &str, outline: &FileOutline) -> Result<()>;
    pub fn remove_file(&mut self, path: &str) -> Result<()>;
    pub fn resolve_calls(&mut self) -> Result<()>;
}
```

### `sync.rs`

```rust
pub struct IgnoreRules { pub respect_gitignore, pub custom_ignore_files, pub max_file_bytes }
pub struct SyncReport { pub scanned, extracted, skipped_unchanged, skipped_too_large,
                         removed, forced_reextraction } // Serialize
pub fn sync_tree(store: &mut CodeStore, root: &Path, rules: &IgnoreRules) -> Result<SyncReport>;
```

### `query.rs`

```rust
pub struct SymbolHit { id, path, name, qname, kind, lang, start_line, end_line,
                        signature, doc, parent_qname } // Serialize
pub struct CallEdge { caller: SymbolHit, callee: SymbolHit, call_text: String, line: u32 }
pub struct AmbiguousCall { caller: SymbolHit, callee_text, line, candidates: Vec<SymbolHit> }
pub struct CallGraph { edges: Vec<CallEdge>, ambiguous: Vec<AmbiguousCall> }
pub struct DocRef { path: String, heading: String, line: u32 }
pub trait DocMentions { fn chunks_mentioning(&self, names: &[&str]) -> Vec<DocRef>; }
pub struct ImpactReport { touched, callers, doc_refs, unresolved_files }

pub fn find(store: &CodeStore, name: &str) -> Result<Vec<SymbolHit>>;
pub fn body(root: &Path, hit: &SymbolHit) -> Result<String>;
pub fn callees(store: &CodeStore, id: i64, depth: u32) -> Result<CallGraph>;
pub fn callers(store: &CodeStore, id: i64, depth: u32) -> Result<CallGraph>;
pub fn path(store: &CodeStore, a: i64, b: i64, max_depth: u32) -> Result<Option<Vec<CallEdge>>>;
pub fn impact(store: &CodeStore, diff_text: &str, depth: u32,
              docs: Option<&dyn DocMentions>) -> Result<ImpactReport>;
pub fn doc_links(docs: &dyn DocMentions, name: &str, qname: &str) -> Vec<DocRef>;
```

## Design choices

- **`upsert_file` takes `lang: Lang` as an explicit parameter.** `FileOutline`
  does not carry the file's language (only `Symbol.lang` exists, and it may be
  absent if the file has no symbols); `sync_tree` already knows `Lang` via
  `Lang::for_path` before calling `extract`, so I pass it through rather than
  deducing it from a symbol that may not exist.

- **Call resolution (`resolve_calls`)**: rebuilds `callee_id` for *all* calls
  on every run (starting from `NULL`), in tiers **same file → imported files →
  same directory → whole repository**, in that order, stopping at the first
  tier that decides: for `a.b()`, it first tries `qname == "a.b"` restricted to
  the tier, then (if no result) the terminal name `"b"`; a tier with ≥ 2
  candidates stops the search (ambiguous, `callee_id = NULL`) without widening,
  since widening could only add candidates, never remove any.

- **Resolving imports to concrete paths** (`candidate_paths_for_import`) is
  heuristic and openly so: relative paths (`./a`, `../b/c`) are joined to the
  caller's directory and then tried with the known language extensions and
  index names (`mod.rs`, `index.ts`, `__init__.py`...); dotted targets
  (Java/Kotlin/C#/Go modules) are compared as path suffixes. Without knowledge
  of the *build systems* (Cargo workspaces, `tsconfig` paths, `go.mod`, Java
  classpath...), some real imports will not be resolved and will fall back to
  the "same directory" or "whole repository" tier.

- **`find`**: 4 strict tiers in the order given by the doc. The last one
  ("case-insensitive") is done **on the Rust side** with `str::to_lowercase()`
  (full Unicode), not via SQLite `LOWER()`/`LIKE` (ASCII folding only): tested
  with `Être`/`être` (the accented `Ê` is not an ASCII letter). `LIKE` patterns
  are escaped (`escape_like`) so that `%`/`_` in a symbol name are not
  interpreted as wildcards.

- **`callers`/`callees`**: classic BFS, a visited-node set prevents any loop on
  a call cycle, bounded by `depth` (0 → empty graph). Unresolved calls
  (`callee_id IS NULL`) met while walking forward (`callees`) are returned in
  `CallGraph::ambiguous` with, for display, the list of symbols sharing their
  terminal name across the whole repository (a search separate from the tiered
  resolution logic of `resolve_calls`, deliberately simpler because purely
  informative).

- **`path`**: forward BFS with a predecessor table, path rebuilt by walking
  back from `b`. Follows resolved edges only.

- **`impact`**: minimal `git diff -U0` parser. `+++ b/<path>` headers select the
  current file (`/dev/null` → deleted file, ignored), `@@ -a,b +c,d @@` hunks
  give the line interval on the new-file side. A pure-deletion hunk (`d == 0`)
  is anchored on line `c` (the line around which the deletion happened) for
  lack of better: there is no "added" line to point to in the final file. Diff
  paths absent from `code_files` (file never synced, or not code) are listed in
  `unresolved_files` rather than silently ignored. `doc_links` is wired through
  an `Option<&dyn DocMentions>` parameter: `None` today (nobody implements it
  yet on the docs side), the report stays consistent (`doc_refs` empty).

- **`DocMentions`**: minimal trait defined in `query.rs`, decoupled from any
  concrete type of the document index. The real implementation (probably on the
  FTS/docs chunks) will be plugged in later without touching this module.

- **`remove_file`** does not delete the `code_calls` rows that *point to* the
  symbols of the removed file: it sets them back to `callee_id = NULL` (they
  become unresolved again) rather than erasing them, so the information "this
  caller called something here" does not silently disappear.

## Known limitations

- `resolve_calls` recomputes all resolutions on every sync (reset then full
  recompute), with no incremental resolution per modified file: simple and
  correct, but costly on a very large monorepo (tens of thousands of symbols).
  To evolve if needed: only recompute the calls whose caller file (or one of
  the files it imports or that import it) has changed.
- A `Call.caller_qname` that matches no symbol of the same file is silently
  dropped on insertion (`code_calls.caller_id` is not nullable): in practice
  this should only happen if the extractor produces a module-level call
  (outside any function/method) with no enclosing symbol.
- Import resolution is a heuristic path-matching algorithm, not real module
  resolution (no reading of `Cargo.toml`/`tsconfig.json`/`go.mod`/classpath);
  imports through complex aliases (`@/components/...`, Cargo workspace paths,
  `PYTHONPATH`...) will not be resolved and will fall back to a wider tier.
- `impact` only interprets the `+++ b/...` header and the `@@ ... @@` hunks:
  any diff that is not strictly unified (e.g. pure renames without `-U0`,
  file-mode-only diffs) is simply ignored for that file rather than making
  parsing fail.
- `body` rereads the whole file on every call (no cache); enough for occasional
  MCP/CLI use, to revisit if called in a tight loop.

---

# Report: MCP server (`src/mcp/`) & client configuration (`src/setup/`)

Implements the in-house MCP server (§4 of `docs/v3/SPEC.md`) and the
generation/merging of client configurations (§3.1). No file of `src/core/` or
`src/main.rs` was modified; `src/lib.rs` only received the two declarations
`pub mod mcp;` / `pub mod setup;`. The real docs engine (DocEngine) is being
developed in parallel by another team: `src/mcp/` only depends on the
`DocsBackend` trait defined here, which that team will plug in.

## Delivered files

- `src/mcp/jsonrpc.rs`: JSON-RPC 2.0 frame types (`RawRequest`,
  `is_notification`), `ok_response`/`error_response` constructors, standard
  error codes.
- `src/mcp/budget.rs`: tokens→characters conversion (§4.3, ~4 chars/token),
  text/line truncation with an explicit truncation notice
  (`with_truncation_notice`), `bound_values` to pack a list of `Value`s into a
  character budget without ever cutting an element.
- `src/mcp/args.rs`: extraction of typed `serde_json::Value` arguments
  (`require_str`, `opt_str`, `opt_u32`, `opt_str_array`, `clamp_depth`).
- `src/mcp/server.rs`: `McpServer`: workspace registry (name → root +
  `CodeStore`), line-by-line `serve()` loop over any `BufRead`/`Write` (hence
  testable in memory via `io::Cursor`, without real stdio), `initialize`
  (protocol version negotiation), `ping`, `tools/list`, `tools/call`, workspace
  resolution (`workspace: Option<&str>`; unknown name → error listing the
  available workspaces).
- `src/mcp/tools/mod.rs`: `ToolSpec`, `ToolOutcome`, `registry()` (merging the
  code + docs tools), `call()` (dispatch by name), `instructions()`.
- `src/mcp/tools/code.rs`: the six code tools, on top of the API already
  delivered in `src/code/` (store/sync/query, see the previous section of this
  report):
  - `code_symbol` (find→0/1/N resolution, body bounded by a line window and/or
    a token budget, truncation indication);
  - `code_callers` / `code_callees` (depth 1 to 3);
  - `code_path` (shortest call path between two symbols);
  - `code_impact` (diff supplied as an argument, or computed via
    `git diff -U0 <base> <head>`; exit codes 0/1 are treated as success, only
    a code >1 is a real error);
  - `code_sync` (full resync via `sync_tree`, or partial on a list of paths via
    `upsert_file`/`remove_file` + `resolve_calls`).
- `src/mcp/tools/docs.rs`: `DocsBackend` trait (6 methods, each
  `(&self, root: &Path, args: &Value) -> Value`, deliberately untyped on the
  parameter side so its shape does not have to change when the other team plugs
  in DocEngine), the `search`/`context`/`inventory`/`map`/`status`/`annotate`
  tools, token-budget pagination for `context` (§4.3: entry clusters packed
  page by page, ledger `current_page`/`total_pages`/`total_entries`/
  `has_more`/`tokens_used`/`token_budget`/`excluded_entities[]`/
  `next_page_command`), test `FakeDocsBackend` (`#[cfg(test)]`, deterministic
  fixture of 5 entries).
- `src/mcp/mod.rs`: submodule wiring, re-exports of `McpServer`,
  `WorkspaceConfig`, `DocsBackend`.
- `src/setup/clients.rs`: `ClientSpec` catalogue of the 11 rows of the §3.1
  table (Claude Code, Cursor, VS Code, Gemini CLI, Antigravity project/user,
  Zed, Generic, Codex, Windsurf, Claude Desktop): scope (`Project`/`User`),
  format (`Json`/`Toml`), relative config path, root key path, entry shape
  (`Standard`/`VsCodeStdio`/`ZedCustom`), project detection marker.
  `claude_desktop_relative_path(os)` takes the OS as an explicit parameter
  (rather than reading `std::env::consts::OS` internally) to stay testable on
  all three branches from any host. `build_entry` builds the JSON entry
  (`["serve"]` at project scope, `["serve", "--path", repo_root]` at user
  scope). `detect_project_clients` serves the `--agent all` detection.
- `src/setup/merge_json.rs`: pure JSON merge: `parse_root_object` explicitly
  rejects a non-object root without writing anything, `merge_entry`/
  `remove_entry` only touch the `documentum` key at the end of the key path,
  creating missing intermediate objects and refusing (without modifying `root`)
  if an intermediate key already exists in another shape. `apply()`
  orchestrates and returns `(changed, new_text)`: `changed = false` on a rerun
  with no semantic change, which is used upstream to never write a backup on a
  no-op.
- `src/setup/merge_toml.rs`: same contract as `merge_json.rs` but for Codex
  (the only TOML client), via `toml_edit::DocumentMut`: preserves the comments
  and formatting of the rest of the file, with the same guarantees of
  non-destruction/idempotence/explicit refusal.
- `src/setup/backup.rs`: `write_with_backup(config_path, new_content,
  backup_dir, timestamp)`: backup to `<backup_dir>/<filename>.<timestamp>.bak`
  before any write that changes the content, no backup if the file did not
  exist yet. `backup_dir` and `timestamp` are always injected parameters: this
  module never resolves a cache directory nor reads the clock itself.
- `src/setup/rules.rs`: block delimited by the markers
  `<!-- documentum:begin -->`/`<!-- documentum:end -->`, `upsert_block`
  (create if absent, replace only the content between markers otherwise,
  append at end of file if the file exists but has no block yet),
  `agents_md_body()` (content of the `AGENTS.md` block), `pointer_body()` (a
  single `@AGENTS.md` / `@./AGENTS.md` line for the `CLAUDE.md`/`GEMINI.md`/
  `.github/copilot-instructions.md` pointer files).
- `src/setup/apply.rs`: `apply_client(spec, base_dir, backup_dir, timestamp,
  command, repo_root, uninstall) -> Outcome`: orchestrates path resolution,
  reading, JSON or TOML merge depending on `spec.format`, and writing with
  backup. `Outcome` ∈ `Installed{backup}` / `AlreadyUpToDate` /
  `Uninstalled{backup}` / `NothingToUninstall` / `Rejected(reason)`.
- `src/setup/mod.rs`: submodule wiring, re-exports.

119 new unit tests (`cargo test --lib mcp::` → 79, `cargo test --lib setup::` →
40), `cargo clippy --lib` and `cargo fmt` clean on these files (same caveats as
the previous section: the remaining clippy warnings in the crate are in
pre-existing files out of scope). Full `cargo test --lib` suite: 334 tests, 0
failures, no regression on the pre-existing `server::mcp::tests::*` tests
(forbidden server, not modified).

## Documented decisions / simplifications

- **Tool names**: `code_*` on the code side, `search`/`context`/`inventory`/
  `map`/`status`/`annotate` on the docs side.
- **`DocsBackend`** takes `(&self, root: &Path, args: &Value) -> Value` for each
  of its 6 methods rather than typed parameters: the real implementation
  (DocEngine, written by the other team) can read additional fields in `args`
  without ever having to change the trait's shape. `FakeDocsBackend` (test
  only) serves as a deterministic double for the pagination and dispatch tests,
  with no pretence of reflecting the real engine.
- **`context` budget**: generic pagination implementation (flat `entries[]`
  list) rather than the `specifications[]`/`code_symbols[]` split with shared
  budgets described in the detail of the spec: a deliberate simplification,
  consistent with the fact that code co-retrieval is done here through the
  dedicated `code_*` tools rather than through the same `context` entry point.
- **`bound_values`** approximates an element's size by the byte length of its
  JSON serialization (not a real token count), and always keeps at least one
  element even if it alone exceeds the budget, so as never to return a strictly
  empty page.
- **`run_git_diff`** (in `code_impact`) treats exit codes 0 *and* 1 of
  `git diff` as success (0 = no difference, 1 = differences found); only a code
  >1 is surfaced as a real error.
- **`extract()` remains a stub** on the `src/code/` side (other team): the
  `code.rs` tests that need resolved symbols/calls build a `FileOutline` by
  hand and call `CodeStore::upsert_file` + `resolve_calls` directly (same
  convention as the `src/code/sync.rs` tests), while also writing the real
  source text to disk so that `code_symbol` (which rereads the file) has real
  lines to slice. The two `code_sync` tests therefore only check the mechanical
  effects of the sync (counters, file presence), not symbol resolution.
- **`detect_project_clients`** explicitly excludes `"generic"` (a pure alias of
  `"claude"`: same `.mcp.json` file, same `mcpServers` key) to avoid detecting
  and applying the same write twice during `--agent all`; `"generic"` remains
  usable by explicit targeting for a client with no dedicated row in the §3.1
  table.
- **Backup directory/timestamp and base directory** are always parameters
  (`base_dir`, `backup_dir`, `timestamp`): `src/setup/` resolves neither the
  `HOME`/cache directory, nor the system clock, nor the list of clients to
  apply for `--agent all`/`--uninstall`; this CLI wiring (flags, path
  resolution, `documentum.toml`) is left to the team rewriting `src/cli/`
  (out of scope for this mission).
- **Content of the `.github/copilot-instructions.md` block**: not detailed by
  the spec beyond the pointer principle; treated like the other pointer files
  (`@AGENTS.md`) for consistency, to be adjusted if the spec specifies a
  different format.

## Known limitations

- `McpServer::serve` only handles a line-delimited stream (one JSON request per
  line): a valid multi-line JSON-RPC message that is not compacted onto a
  single line would be split wrongly. Consistent with most real stdio MCP
  clients, which send one line per message.
- `apply_client`/`merge_json`/`merge_toml` do not validate the *content* of
  neighbouring keys beyond "is it an object/a table": a config file that is
  otherwise invalid for the target client (wrong value type in a neighbouring
  entry, etc.) will not be detected here; only the path we touch is validated.
- No `setup`/`mcp serve` CLI command is wired by this module: it only provides
  the pure building blocks (catalogue, merge, backup, rules blocks);
  integration (flags, `$HOME`/cache resolution, choice of clients for
  `--agent all`) falls to the CLI team.
