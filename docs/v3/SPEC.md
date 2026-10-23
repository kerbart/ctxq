# documentum v3: functional specification

This document specifies the **behaviours and contracts** of the `documentum`
binary: what it does, not how the code is organised. The code graph is
described in `docs/v3/CODEGRAPH.md`.

Main modules: `src/core/{secrets,ingest,convert,lexical,docmeta,settings,
mdchunk,agent,embed_http,collect,doctext}.rs`, `src/core/sources/*`,
`src/core/store/*`, `src/cli/daemon.rs`.

No machine-, model- or service-specific value in the code: everything goes
through `documentum.toml` (documented template: `documentum.example.toml`).

---

## 1. Document pipeline

### 1.1 File discovery (`[collect]`, module `collect.rs`)

A discovery filter resolves, from the workspace root and `[collect]`:

- **Roots** (`roots`, default `["."]`): indexed directories, **in priority
  order**: when two chunks have identical content, the one whose path belongs
  to the root declared first survives deduplication. The rank of a relative
  path in the list of roots must be computable independently (pure function,
  testable outside any I/O context): exact prefix or `root/`; the empty root
  or `"."` matches everything, as a last resort.
- **Extensions** (`extensions`, default empty = every supported type).
- **Size limits**: `max_file_bytes` (text, default 1 MiB) and
  `max_document_bytes` (docx/pptx/xlsx/pdf, default 50 MiB, measured on the
  source file before conversion: a slide deck is a zip of images around a
  little text).
- **Directory exclusions**: `exclude_dirs` (exact name, any depth),
  `exclude_dir_names` (same, case-insensitive; default
  `_archive, archive, archives, backup, backups`), `exclude_top` (top-level
  directories of the root only: a deeper subdirectory with the same name is
  not affected).
- **Exclusion globs**: file `exclude_globs_file` (default
  `.documentumignore`, one line = one glob relative to the root, `#` =
  comment, leading/trailing slashes ignored) **plus** inline `exclude_globs`;
  a pattern `x` also excludes `x/**`. A missing `.documentumignore` file
  causes no error (empty list).
- **`.gitignore`**: `respect_gitignore` (bool, default `true`): delegates to
  the file walker (an `ignore`-style walker: honours `.gitignore`, `.ignore`,
  `.git/info/exclude` at every level).
- **`own_remotes`** (list of substrings, default empty = rule disabled): a
  subdirectory that is **not a root** and **not a direct child of the root**
  and that contains its own `.git/config` with a `remote` URL matching **none**
  of the `own_remotes` substrings is a third-party clone (vendored dependency,
  cloned fork) and is excluded entirely. A top-level directory of the root
  (its parent is the root) is **never** considered a clone: it is one's own
  project. A `.git` that is a **file** (worktree, submodule) does not trigger
  the rule: it is kept. If `own_remotes` is empty, no nested git directory is
  excluded by this rule (ownership is never guessed).
- **Always-excluded files**: `documentum.toml`, `.documentum.toml`,
  `.documentumignore` themselves (configuration, not knowledge).
- **`table_row_dirs`**: directories (relative to the root) whose large
  markdown tables are *additionally* indexed row by row (see §1.2). The scan
  output directory (`[scan].output_dir`) is always implicitly added to it.

Required behaviour: the predicates "exclude this directory" and "exclude this
file (given its size)" must be computable independently of any real disk
access (testable with a synthetic temporary directory), and the rank of a path
among the declared roots must be a pure function
`(roots, relative_path) -> rank` (smaller = higher priority; not in the list =
last rank).

### 1.2 Markdown chunking: two chunkers

There are **two** markdown splitters, with different roles:

**A. Primary chunker for `.md`/`.markdown` documents and converted documents
(docx/pptx/xlsx/pdf → markdown)**. It carries the golden relevance score
(MRR ≈ 0.79). It is therefore a **critical** component, and its behaviour is
fixed as follows:

- Splits only at **level 2 and 3** headings (`##`, `###`), detected by a
  full-line regex `^#{1,6}\s+(.*?)\s*#*$` on text **already split into logical
  lines** (handles `\n` and `\r\n`).
- The preamble before the first heading becomes a "document" chunk whose title
  is the document title, itself taken from the first `# title` (level 1) found
  in the file, or failing that the file name without extension.
- Breadcrumb (separator `" > "`, constant shared with the lexical channel):
  `[doc_title]` for the preamble section; `[doc_title, h2_title]` for a level-2
  section; `[doc_title, last_h2_title_seen, h3_title]` for a level-3 section
  (if no H2 came before, `[doc_title, h3_title]`). An "orphan" H3 (without a
  parent H2) must never crash the breadcrumb computation.
- Each section runs from its heading to the next level-2 or level-3 heading
  (or end of file); a section whose text is empty after `trim` is
  **dropped** (no empty chunk).
- `start_line`/`end_line`: 1-indexed, bounds of the range of lines covered by
  the section in the source file (not in the chunk text).
- **Row-by-row table indexing** (only if the file is in a `table_row_dirs`, or
  if the source extension is `.xlsx`): a markdown table (a `|...|` line, a
  `|---|---|` separator line, then ≥ 4 data lines; below that the table stays a
  single chunk) produces, **in addition** to the section that contains it, one
  chunk per data row. For each row: the chunk title is the content of its
  **first cell** (`row N` if the cell is empty); the breadcrumb is that of the
  section but its **last link** is completed with `" | " + cell` (not an extra
  link); the chunk content is `header\nseparator\nrow` (3 lines, so the text
  stays readable on its own); `start_line`/`end_line` point at the exact data
  line. Output order: the section first, then its row chunks.
- `symbol_references` of each chunk: result of the symbol extractor (§1.3)
  applied to the chunk text.
- `id` of each chunk: deterministic identifier (§1.3) over `(relative path,
  section title, increasing chunk index in the file)`.
- A **chunker version** must be persisted in the index metadata (dedicated
  key): on opening, if the stored version differs from the current version (or
  is absent while the index already contains chunks), a full reindex must be
  triggered at the next sync. An unchanged file hash is not enough to detect
  this, since the file has not changed, only the way it is split has.

**B. Generic fallback chunker**: for any text file that is **neither**
recognised as SQL, workflow, spec, infra, manifest, API contract, config,
**nor** `.md`/`.markdown`/converted (that is: raw text files with no
recognised extension, `.txt`, etc.). It must:
- Return an empty `Vec` for empty/blank content.
- Parse the text as generic markdown (full AST: headings of *all* levels H1-H6,
  tables, code blocks, lists, YAML `---` front matter), with lines located by
  **exact byte offset** (binary search on the positions of `\n`), so that code
  blocks (including their content, even if it looks like a markdown heading: a
  `# comment` inside a ```bash``` block must **never** be treated as a heading)
  and tables (cells joined by `" | "`) do not break the section hierarchy.
- Hierarchy through a **heading stack**: a non-standard level jump (H1 → H3
  directly) must produce a `[H1, H3]` hierarchy, not an error.
- A section with no body before the next heading must still produce a chunk
  (the title alone as content), except the very last empty section at the end
  of the file, which is dropped unless it is the document's only chunk.
- `start_line` must stay correct even with CRLF line endings.
- A document entirely without headings produces a single chunk
  `section_title = "Overview"`.
- Must also provide `generate_chunk_id` and `extract_symbol_references`
  (§1.3, shared with chunker A).

**Data contract common to both chunkers: `DocChunk`** (consumed as is by
`lexical.rs`, `mdchunk.rs` and the hybrid engine):

| field | type | semantics |
|---|---|---|
| `id` | `String` | deterministic identifier, §1.3 |
| `source_path` | `String` | path relative to the root, `/` separators |
| `section_title` | `String` | section title (or table cell for a row chunk) |
| `heading_level` | `u32` | heading level (1-6; 0 for a pseudo-section) |
| `hierarchy` | `Vec<String>` | breadcrumb, from the document to the section |
| `content` | `String` | raw chunk text (re-rendered markdown or extracted as is, depending on the chunker) |
| `start_line` / `end_line` | `usize` | 1-indexed, inclusive |
| `symbol_references` | `Vec<String>` | symbol names detected in the text, normalised (§1.3) |

`DocChunk` is serialisable (JSON) and comparable by structural equality.

### 1.3 Symbol extraction and deterministic identifiers (shared contract)

Three pure functions, used by both chunkers and by the doc↔code linker:

- **`symbol_ref(name) -> String`**: trim, then if the name starts with `@` and
  contains an alphanumeric `kind:` prefix (`@symbol:`, `@service: `, `@fn:`,
  `@interface:`, `@class:`, `@module:`...), returns the part after the colon,
  trimmed; otherwise returns the trimmed name as is.
- **`extract_symbol_references(text) -> Vec<String>`**: the union, in order of
  first appearance, without duplicates, of:
  1. every `@kind: Name` where `kind` ∈ {service, symbol, fn, func, interface,
     class, struct, module} and `Name` matches `[A-Za-z0-9_.:\-]+`, passed
     through `symbol_ref`;
  2. every identifier between backticks `` `Name` `` where `Name` looks like
     `[A-Za-z_][A-Za-z0-9_\-]*` optionally followed by `.ident` or `::ident`
     segments, **AND** judged plausible by the following rule: length > 1 and
     (contains a `.`, a `::`, a `_`, OR an uppercase letter). This rule
     eliminates common words between backticks (`true`, `false`, `null`,
     `npm`) while keeping `PaymentProcessor`, `order_total`, `config.port`,
     `std::fs`.
  An empty name after normalisation is never added.
- **`generate_chunk_id(source_path, section_title, index) -> String`**: a
  deterministic, stable hash (same machine, same run, same inputs → always the
  same output; two different indexes → different outputs) of the three inputs
  concatenated unambiguously, rendered as fixed-length hexadecimal (16
  characters). The hash need not be cryptographic; it must be stable **across
  launches** (not based on per-process randomness).

### 1.4 SQLite database: logical schema (a single database for docs + code + cache)

The document engine persists into a single SQLite database (`docs.db`),
alongside the code-graph tables (see CODEGRAPH.md).
Logical schema:

- **`doc_chunks`**: `id` (text primary key), `source_path`, `section_title`,
  `heading_level` (integer), `content`, `start_line` / `end_line` (integers),
  `symbol_references` (text, serialised JSON), `hierarchy` (text, serialised
  JSON). Index on `source_path` (for per-file deletions and "first chunk of
  this file" queries).
- **`chunks_fts`**: **FTS5** virtual table, tokenizer `porter unicode61`,
  indexed columns `source_path`, `section_title`, `content`, `hierarchy` (the
  path and the breadcrumb must be searchable: they often carry the project
  name, absent from the body). Non-indexed `id` column (`UNINDEXED`) for the
  join to `doc_chunks`.
- **`chunk_vectors`**: `chunk_id` (primary key, references `doc_chunks.id`),
  `vector` (BLOB: concatenated little-endian 32-bit floats).
- **`embed_cache`**: composite key `(embedder, digest)`, `vector` column
  (BLOB). `embedder` identifies the space (e.g. `"query:<embedder_id>"` for
  query vectors, vs the document space); `digest` = stable fingerprint of the
  text submitted to the embedder (e.g. SHA-256 hex). Used both to avoid
  recomputing the embedding of an unchanged chunk, and to cache the vectors of
  repeated **queries** of a remote embedder (a network round trip costs ~200 ms
  against a nearly free cache hit).
- **`doc_file_meta`**: `source_path` (primary key), `mtime_millis` (integer:
  the file's last-modification timestamp in epoch milliseconds),
  `content_hash` (integer: content hash, to detect a content change without an
  mtime change, and conversely an mtime change without a content change,
  typically after a `git checkout`).
- **`index_meta`**: generic key/value table (`key` primary, `value` text)
  carrying at least: schema version, identifier and dimension of the active
  embedder, chunker version (§1.2), workspace root associated with this index
  (so settings can be reloaded from the right place), timestamp of the last
  successful sync.
- **`doc_meta`**: `source_path` (primary key), `meta` (JSON text, see §1.5
  `DocMeta`). Rewritten entirely at each sync (a sync rewrites the metadata of
  every file seen).
- **`doc_annotations`**: `source_path` (primary key), `summary` (text,
  nullable), `tags` (JSON text), `author` (text), `updated_at` (text, ISO
  timestamp). Survives between two syncs as long as the file exists (deleted
  only when the file disappears from the index): this is what an agent learned
  about a document, never written into the source file.

**Incremental sync**: a file is re-extracted (re-chunked, re-embedded) only if
its `mtime_millis` **or** its `content_hash` changed relative to
`doc_file_meta`, **or** if the chunker version stored in `index_meta` differs
from the current version (full reindex in that case, all files together, not
just incremental), **or** if the stored embedder identifier differs from the
active one (incompatible vector dimension/semantics: full re-embedding
scheduled at the next sync; search must refuse to serve results with
inconsistent vectors and return an explicit error inviting a rerun of
`sync`). Files that disappeared since the last sync must be purged from
`doc_chunks`, `chunk_vectors`, `doc_file_meta`, `doc_meta` and from any active
external vector store.

**Completeness accounting** (feeds `doctor`/`status`): every file seen during a
sync must end up in one of the categories: *indexed* (≥ 1 chunk produced),
*empty* (0 chunks, legitimately empty file), *quarantined* (secret detected,
never chunked nor embedded; see `secrets.rs`), *unusable*
(unreadable encoding, non-convertible format...), or else *unaccounted*. That
last category is a bug (lost document) that `doctor` must be able to report
explicitly.

### 1.5 Document metadata and filters (`DocMeta`, `SearchFilters`)

Module `docmeta.rs`. Data contract:

`DocMeta`: `title` (optional), `summary` (optional), `tags` (list, possibly
empty), `date` (optional, `YYYY-MM-DD`, truncated to 10 characters if longer
in the source), `source_type` (always set: declared in the front matter,
otherwise derived from the extension: `md`/`markdown`/`mdx` → `markdown`,
`yml` → `yaml`, no extension → `text`, otherwise the extension as is),
`category` (optional), `annotated_by` (optional, `"file"` or `"agent"`).

Front-matter source: a YAML block `---\n ... \n---` at the top of the file
(UTF-8 BOM tolerated before it), keys `title`, `summary`/`description`
(fallback), `category`, `date`, `source_type` (overrides the derived type),
`tags` (YAML sequence, or a single string split on `,`). A file without valid
front matter produces a default `DocMeta` with only `source_type` set (never
an error).

`SearchFilters` (deserialisable from an agent tool's parameters, flattened onto
`SearchArgs`): `tags` (all must be present, case-insensitive), `source_types`
(at least one must match, case-insensitive), `since` (`YYYY-MM-DD`: a
document's effective date is the front-matter one, otherwise the file's
last-modification date; neither present = filter not satisfied),
`path_prefixes` (at least one prefix must match the relative path).
`is_empty()` is true iff all four fields are empty/`None`. `matches(path, meta,
modified)` must evaluate the four criteria independently (short-circuiting on
the first acceptable failure).

### 1.6 Hybrid search: the relevance core

This is the most sensitive section: the constants and formulas below were
measured on a golden set (target MRR **0.793**, see §7) and are not to be
changed without replaying that gate.

**Lexical tokenisation** (module `lexical.rs`; it carries the score):
- A token = a run matching `[a-z0-9]+(?:[._-][a-z0-9]+)*` (compound
  identifiers, case-insensitive after `lowercase`), kept **whole** if at least
  2 characters long.
- In addition to the whole compound identifier, each of its **sub-parts**
  separated by `.`/`_`/`-` is also emitted as a separate token (at least 2
  characters): `app.example.com` → tokens `app.example.com`, `app`,
  `example`, `com`. Goal: a partial query on a single sub-part must match.
- A chunk's lexical text = `source_path\nbreadcrumb\ncontent` concatenated (the
  path often carries the project name, absent from the body).

**BM25: exact constants and formula**:
- `K1 = 1.5`, `B = 0.75` (classic Robertson/Sparck-Jones BM25).
- `idf(df) = ln(1 + (N - df + 0.5) / (df + 0.5))` with `N` = total number of
  chunks in the corpus (after deduplication, see below) and `df` = number of
  chunks containing the term.
- Score of a term for a chunk `i` of frequency `f`, length `dl_i`, with corpus
  average length `avgdl`:
  `idf(df) * (f * (K1 + 1)) / (f + K1 * (1 - B + B * dl_i / avgdl))`.
- **Global BM25, inverted index built over the WHOLE corpus** (not just the
  dense pool): an identifier absent from the best dense candidates must remain
  findable by global BM25. This is the dominant query profile of an
  infrastructure knowledge base (a port, a container name, a domain).
- **Normalisation by an ABSOLUTE ceiling, not relative to the pool**:
  `bm25_max(query) = Σ idf(df_t) * (K1 + 1)` over the query terms present in
  the corpus (TF saturated at infinity, hypothetical shortest possible
  document). Normalising by the maximum *of the candidate pool* instead of this
  absolute ceiling would give a full bonus to a mediocre match as soon as the
  pool contains nothing better, which demotes good semantic top results. This
  error was measured and is avoided by design.

**Score fusion (default mode, `[search] fusion = "score"`)**:
```
score(chunk) = cosine(query, chunk) + alpha * bm25(chunk) / bm25_max(query)
```
where `alpha` depends on the nature of the query:
- `alpha_id` (default **0.5**) if the query is judged an "identifier query";
- `alpha` (default **0.2**) otherwise (natural-language question).

**"Identifier query" detection**: true if (a) the query contains a digit, OR an
`alphanumeric [._@-] alphanumeric` sequence (pattern
`(?i)\d|[a-z0-9][._@-][a-z0-9]|\.service\b`), OR (b) the query contains a word
from the `lookup_terms` list (configurable, replaces the default list if
provided; default list bilingual FR/EN: port(s), ip, pin,
adresse(s)/address(es), domaine(s)/domain(s), sous-domaine(s)/subdomain,
hostname(s), conteneur(s)/container(s), unité(s)/unit(s), systemd, url,
chemin/path). Goal: a question like "which port does the budget app use" must
route to `alpha_id` even if the question itself contains no identifier: the
**expected answer** is one.

**Candidates and ceilings (`[search]`, defaults)**:
- `pool` = **60**: size of the dense pool (cosine candidates).
- `lex_pool` = **40**: extra candidates coming from pure global BM25 (those
  the dense pool missed), added only if `alpha > 0`.
- `max_per_source` = **2**: at most N results per source file in the output
  page (0 = unlimited), so that a single document does not monopolise the
  top-k.
- `dedup_min_chars` = **200**: two chunks whose normalised text (whitespace
  collapsed) is identical and at least N characters long are deduplicated (the
  first in root priority order, §1.1, survives).
- The dense pool must be over-fetched (`limit * 4` if `max_per_source > 0`,
  otherwise `limit`) to survive the per-file cap and faceted filters.
- When filters (`SearchFilters`) are active, the dense pool must be widened
  further (×3) because part of the pool will be eliminated afterwards by the
  filter.

**RRF mode (ablation, `[search] fusion = "rrf"`)**: an alternative option, not
the default path: reciprocal rank fusion,
`RRF_K = 60.0` (smoothing constant, Cormack et al. 2009), score =
`Σ_channels channel_weight / (RRF_K + rank_in_channel + 1)`, lexical channel
weight `FTS_CHANNEL_WEIGHT = 1.2` (dense is implicitly 1.0), candidates
over-fetched per channel = `max(limit * 3, 20)` before fusion. Measured on the golden set,
this mode drops recall@1 from 68% to 50% (noisy candidates overtake good top
results), which is why it is not the default.

**Deduplication and final sort**: on equal scores, the order must be total and
deterministic (e.g. tie-break by increasing index/identifier): two runs on the
same index must produce exactly the same ranking.

**Faceted filters**: an "allowed file" mask is decided **once per file** (not
per chunk) from `SearchFilters::matches`; a chunk of a rejected file is
excluded from the pool before score computation, not after.

**Federation (`[[store.federate]]`)**: additional read-only stores (Qdrant or
documentum peer) are queried on every search, **in parallel** with the local
index, with:
- a purely dense score (no local lexical evidence), multiplied by the federated
  store's `weight` (default 1.0);
- a local copy of the same passage (same source + same start line) always wins
  over its federated copy (deduplication by `(source, start_line)`);
- an unreachable federated store is **ignored with a warning**, never fatal:
  federation must never bring down the local answer;
- each result carries an origin label (`origin`, `None` for the local index,
  the store's label/name otherwise);
- final truncation to the requested limit after merge + dedup across all
  sources.

### 1.7 Embedding backends

Common interface (`Embedder`): `embed_documents(texts) -> Vec<Vec<f32>>`,
`embed_query(text) -> Vec<f32>`, `id() -> String` (stable identity that encodes
everything that changes the vector semantics: backend + model + task
templates; changing one without rebuilding the index must be detectable),
`dim() -> usize`, `cosine_similarity`.

- **Local (default, offline)**: deterministic hashing of character n-grams +
  words, **128-dimension** dense vector, L2-normalised. Algorithm: for each cleaned word (alphanumeric + `_`/`.`), a word
  hash weighted ±1.5 on a bucket `hash(word) mod 128` (sign drawn from a second
  hash of the word suffixed `_sign`), character trigrams each weighted ±0.5 on
  `hash(trigram) mod 128` (words ≥ 3 characters), and consecutive-word bigrams
  weighted ±1.0 on `hash(bigram) mod 128`; final vector L2-normalised (division
  by the norm if > 1e-6, otherwise zero vector). Stable identifier:
  `"local-hash-v1-128"` (bumped if the hashing scheme changes). The hash
  used must be **deterministic across launches** (not a random per-process hash
  like Rust's default `HashMap` `SipHash`).
- **Ollama** (`POST /api/embed`, batched) and **OpenAI-compatible**
  (`POST /v1/embeddings`; also applies to LM Studio, llama.cpp server, vLLM,
  text-embeddings-inference, Infinity): several URLs can be declared, tried in
  order, and the first to answer becomes preferred for subsequent calls
  (fast-server → local fallback switch). Task templates (`{t}` = document text,
  `{q}` = query text) are applied **before** sending; omitting them silently
  degrades relevance, no error is raised. Families recognised by substring of
  the model name with default templates: `nomic` (`search_document: {t}` /
  `search_query: {q}`), `qwen3` (document passthrough, query with an explicit
  retrieval instruction), `e5` (`passage: {t}` / `query: {q}`),
  `embeddinggemma`, `arctic-embed`, `bge`; any other model without a template
  declared in `[embed.templates.<substring>]` goes *passthrough* (`{t}`/`{q}`
  as is). Deterministic truncation in characters (`max_chars`, default 6000)
  before sending: a server silently truncates or rejects depending on the
  implementation, and client-side truncation guarantees a reproducible index.
  Concurrency (default 1: a shared server must not be starved by a rebuild) and
  batch size (default 8) are bounded. On a "context too long" error, fallback:
  retry text by text, halving the length of the stuck text until success or
  200 characters. In-memory query cache (LRU, ~1024 entries) on top of the
  persistent SQLite cache (`embed_cache`), because an agent often repeats the
  same questions. Embedder identity = `"http:<model>:<hash of the two
  templates>"` (the model AND the templates make up the identity: changing
  either without rebuilding would yield wrong results).
- **ONNX** (optional compile-time feature `onnx-embeddings`): model
  `bge-small-en-v1.5`, **384 dimensions**, downloaded **on demand** at first
  use (never at build or install time), cache directory resolved by precedence
  `$DOCUMENTUM_ONNX_CACHE` > `$FASTEMBED_CACHE_PATH` >
  `$DOCUMENTUM_HOME_DIR`/`$HOME`/`%USERPROFILE%` + `/.documentum/models` >
  system temporary directory. Stable identifier `"bge-small-en-v1.5-384"`. A
  loading/inference error must never panic: fall back to a zero vector with a
  logged warning.

Backend selection: `[embed].backend` (`local`|`onnx`|`ollama`|`openai`, default
`local`), overridable by `DOCUMENTUM_EMBEDDINGS`/by the global CLI flag
`--embeddings`. No index must ever be rebuilt **implicitly** with a remote
embedder (non-consented cost and network dependency): a backend change
detected on opening schedules a full re-embedding at the **next explicit
`sync`**, never automatically in the course of a search.

---

## 2. Inventories, sources, scan, ingest, convert, export-map, store, daemon

This section covers the CLI/config interface of these features; the
implementation detail is in the code (`sources/*`, `ingest.rs`, `convert.rs`,
`collect.rs`, `settings.rs`, `store/*`, `cli/daemon.rs`).

### 2.1 `[[scan.source]]`: inventories

Each source produces **one dated markdown document**, written under
`[scan].output_dir` (default `docs/generated`), indexed like any other file
(its tables included, row by row). Fields of a source: `name` (file name
without extension, must be a simple name: no `/`, no `\`, not starting with
`.`), `title` (optional, otherwise derived), `kind` (default `"command"`),
`command` (argv, required if `kind = "command"`), `format` (`"code"` default =
output wrapped in a code block; `"markdown"` = inserted as is), `intro`
(paragraph under the title), `timeout_secs` (default 60), free options
specific to the `kind` (flattened TOML table).

Built-in kinds (no external program): `docker` (Engine socket, options
`socket`, `all`), `systemd` (option `user` for `--user` units, `exclude`
replaces the default list), `ports` (listening sockets via `/proc`, option
`udp`), `projects` (subdirectories and their entry document, options `dir`,
`entry_files`, `exclude`), `scripts` (executables and their first comment line,
option `dirs`). Prose generated in `en` or `fr` (`[scan].lang`).

`documentum scan [--only NAME...] [--sync]` runs the declared sources (or only
the named ones), writes each changed document via atomic
write-then-rename (a concurrent sync never reads a half-written file),
continues when a source fails (the previous document stays in place, the
failure is reported), and resyncs the index at the end of the command if
`--sync`.

### 2.2 `ingest` / `export-map`: intake and organize mechanics

These two commands turn a heap of heterogeneous files (office documents,
minutes, mails, exports, repositories) dropped in an intake folder into a
markdown tree plus a manifest that code agents can use. On-disk layout under
the context folder: `.organizer/{inventory,mapping,conversion-log}.json`,
`.organizer/staging/`, `assets/`, `sources/`, `INDEX.md`, `INDEX.json`,
`AGENTS.md`, `.gitignore`.

`documentum ingest [INTAKE=_intake] [--context context] [--force] [--json]`:
inventory by sha256 (duplicates → the most recent wins, top-level git
repositories detected as repos), quarantine (secret scanner + file-name
heuristic + `client_secret`/`private_key` patterns, **and re-scan of the
converted markdown**: a key in a docx is only visible after conversion),
conversion to markdown (plain text directly; docx/pptx/xlsx/pdf via
`convert.rs`; html and eml natively; `.doc/.ppt/.xls/.rtf/.msg/.odt` →
**explicit** failure with a message suggesting an external converter, never a
silently degraded conversion), front-matter stub, asset copy, per-repo pointer
(tree, detected languages, README). **Idempotent by hash**: rerunning without
`--force` does not reconvert what is already in `mapping.json`. JSON output
(`--json`): `kept`, `discarded_duplicates`, `repos`, `converted`, `copied`,
`skipped`, `quarantined`, `failed`, `staging` (path), `repo_pointers` (list),
`quarantine` (list of path/reason pairs), `failures` (list of path/reason
pairs); otherwise an equivalent text report.

`documentum export-map [CONTEXT=context] [--profile pure-code|shaping|swarming]
[--intake _intake] [--force] [--agents-template FILE] [--json]`: builds
`INDEX.md`/`INDEX.json`/`AGENTS.md`/`.gitignore` from the front matter of
`<context>/sources/**/*.md` (`docmeta` reader), cross-checked against the
recorded conversions. Known categories are `decisions`, `specs`, `meetings`,
`mails`, `design` and `misc` (front matter `category: misc`, directory
`sources/misc/`). **Refuses to write** (exit code 1) as long as `staging/`
contains unclassified documents, except with `--force`: losing a document left
in staging would otherwise be silent. JSON output: `written` (bool), `docs`,
`categories`, `leftover_staging` (paths).

### 2.3 `convert`: office documents and PDF

`documentum convert FILE` prints the resulting markdown on stdout.
Supported extensions: `docx`, `pptx`, `xlsx`, `pdf` (constant shared with the
discovery filter, §1.1, to apply the right size limit). Conversion is
**in-process**, with no external dependency (MarkItDown, LibreOffice): direct
reading of the XML parts of the OOXML zip for docx/pptx/xlsx; PDF extracted in
a **dedicated subprocess** (some extraction libraries write to stdout, which
would corrupt the MCP stdio stream if done in the server process). Structured
markdown output: headings, tables (spreadsheets → table, rows truncated to
2000 per sheet: a 100k-row export is data, not knowledge), slides with
speaker notes, a title H1 added if the document does not already provide one,
so that the output then goes through the same markdown chunker as any `.md`
(§1.2.A).

### 2.4 `store`: external vector stores (`[store]`, module `store/*`)

Common interface (`VectorStore`, trait): `label()`, `ensure(dim)` (creates the
collection if needed), `upsert(points)`, `delete_sources(paths)`,
`search(vector, k, optional_restriction_by_id)`, `count()`,
`query(text, vector, k)` (by default delegates to `search`; a federated
store of the peer type may re-embed the text with its own model instead of
using the supplied vector). A `StorePoint` carries everything an external
store must know to answer on its own: `chunk_id`, `vector`, `source`,
`breadcrumb`, `text`, `start_line`, `end_line`.

Backends: `local` (default: vectors in the index's SQLite database, exact
in-memory scan), `qdrant` (HTTP collection, no heavy client), `peer` (another
documentum instance through its daemon HTTP API; federation only, never as the
primary `[store]`). Modes: `owned` (`sync` writes to it), `attached`
(read-only: a database shared by someone else, queried as is).
`[[store.federate]]` declares additional stores always in `attached` mode,
with `weight` and optional `name` (display label, otherwise the URL).

`documentum store push`: pushes all the vectors already present in the local
index to the configured external store, **without re-embedding** (fast store
switch); refuses if the store is `attached` (read-only) or if `[store]` is
`local` (nothing to push). `documentum store status`: number of points on the
external store side + number of chunks on the local index side (to spot a
mismatch), or `backend = local : vectors in <path>` if there is no external
store.

### 2.5 `daemon`: resident process and `search` client (module `cli/daemon.rs`)

Loading the index (chunks, vectors, inverted index) takes ≈ 1 s; a query on a
warm index costs milliseconds. The daemon keeps a warm engine behind a Unix
socket (`daemon.sock`, mode `0600`, in the workspace cache directory by
default, or custom `[daemon].socket`), plus optionally a minimal HTTP API
(`[daemon].http`, e.g. `127.0.0.1:7700`).

**Socket protocol**: one line = one JSON object `{"op": "...", ...}` → one
response line `{"ok": true, "data": ...}` or `{"ok": false, "error": "..."}`.
Operations: `search` (args = serialised `SearchArgs`, see §1.5/§4.3), `status`,
`inventory` (`kind`, `filter`, `limit`), `map` (`prefix`, `depth`, `limit`),
`annotate` (`path` required, `summary`, `tags`, `author` default `"agent"`),
`ping` (`{"pong": true, "pid": ...}`).

**HTTP API** (if configured, optional bearer token via
`[daemon].http_token_env`): `GET /health` (no auth), `GET /status`,
`POST /search`, `POST /inventory`, `POST /map`, `POST /annotate` (JSON body,
same fields as the socket protocol). 401 if a token is expected and missing or
wrong, 404 on unknown route.

**Automatic reload**: the worker holds the engine on a single thread (SQLite
connection, caches); on each request it compares the `last_sync` stored in the
database with the remembered one. If another process (CLI `sync`, scheduled
scan) resynced in the meantime, the engine invalidates its in-memory view
before answering. A *warm-up* (a dummy search) is done at startup so that the
first real query does not pay the loading cost.

**Periodic refresh** (`[daemon].sync_interval_secs` / `scan_interval_secs`,
0 = disabled): in a separate thread, on a distinct engine (queries keep being
served during the resync), with a scan always followed by a sync.

`documentum daemon [--http ADDRESS] [--stop]`: refuses to start if a daemon
already answers on the target socket, or if the index does not exist yet
(invites the user to run `sync` first). `--stop` stops the current workspace's
daemon.

`documentum search QUERY [-l|--limit N] [--full] [--json] [--no-daemon]
[--tag T]... [--type T]... [--since DATE] [--path PREFIX]...` (`--k` is an
alias of `--limit`, default 5): goes through the daemon, started on demand,
except with `--no-daemon` (in-process search, neither starting nor using a
daemon). `--full` disables the truncation of passages. JSON output: see §3.
`q` is a hidden former name of `search`, kept for scripts: `documentum q
[QUERY] [--k N] [--full] [--json] [--stop] [--no-daemon] [--tag T]...
[--type T]... [--since DATE] [--path PREFIX]...`, with `--stop` stopping the
daemon and a JSON output that is a bare list of objects `{rank, score, source,
title, breadcrumb, line_start, line_end, text}`.

---

## 3. CLI: subcommands

Top-level flags: `--path`/`-p` (workspace root, placed before the subcommand; not global because `search` has its own `--path` filter; default current
directory), `--embeddings {local|onnx|ollama|openai}` (overrides
`[embed].backend`), `--help`, `--version`. Subcommands, in `--help` order:

- **`index`**: full reindex of the documentation (and, when `[code] enabled`,
  of the code graph). Progressive text output (numbered steps), final summary
  (files scanned, chunks, any analysis problems).
- **`sync`**: incremental sync of the documentation (and of the code graph).
  Same output as `index`.
- **`doctor [--workspace NAME=PATH]... [--all]`**: health diagnostic: database
  integrity, coverage (indexed/empty/quarantined/unusable/unaccounted),
  freshness (last sync). Read-only. Without `--all`, only the first
  `--workspace` given is inspected; `--all` inspects every one.
- **`daemon [--http ADDRESS] [--stop]`**: see §2.5.
- **`inventory [KIND] [--filter TEXT] [--limit 50]`**: structured rows of an
  inventory (no vector search), or the list of inventories if `KIND` is
  omitted. JSON output.
- **`map [PREFIX] [--depth 2] [--limit 200]`**: tree of one-line summaries of
  the workspace. JSON output.
- **`status`**: freshness, store, sources, coverage. JSON output.
- **`annotate PATH [--summary TEXT] [--tag T]... [--author cli]`**: records an
  agent summary/tags, source file not modified.
- **`store {push|status}`**: see §2.4.
- **`convert FILE`**: see §2.3.
- **`ingest`** / **`export-map`**: see §2.2.
- **`scan [--only NAME]... [--sync]`**: see §2.1.
- **`import-vectors --chunks chunks.jsonl --vectors embeddings.npy`**: seeds
  the embedding cache from an existing index (one JSON line per chunk in
  `chunks.jsonl`: breadcrumb + text; aligned NumPy `.npy`, one row per chunk):
  a first sync with the same model then only re-embeds what the source does
  not cover. Fails explicitly if the counts do not match.
- **`search QUERY [-l|--limit 5] [--full] [--no-daemon] [--tag T]...
  [--type T]... [--since DATE] [--path PREFIX]... [--json]`** (`--k` is an
  alias of `--limit`): search (§1.6, §2.5); the limit is clamped to 1-50. JSON:
  `{query, results: [{rank, score, source, lines: [start, end], title,
  breadcrumb, text, meta, origin}]}`.
- **`mcp [--workspace NAME=PATH]...`** (alias `serve`): MCP server on stdio
  (§4), launched by the client, never by hand. Several workspaces served by a
  single process if `--workspace` is repeated; the first is the default, the
  tools take a `workspace` parameter to target the others. `--workspace` takes
  precedence over `--path`.
- **`setup [--client CLIENT]... [--uninstall] [--command PATH] [--rules[=false]]
  [--skill] [--hooks] [--in-repo]`**: registers the MCP server with clients
  (§3.1). `--client` (alias `--agent`, repeatable) takes `all` or a client
  identifier: `claude`, `cursor`, `vscode`, `gemini`, `antigravity`, `zed`,
  `generic`, `antigravity-user`, `codex`, `windsurf`, `claude-desktop`. The
  default `all` covers detected **project** clients only; **user** clients
  (`antigravity-user`, `codex`, `windsurf`, `claude-desktop`) must be named
  explicitly and receive `--path <repo>` pinned in their config.
  `--uninstall` removes the entry instead of adding it; `--command PATH` is
  the command written in the configs (default: absolute path of the current
  binary); `--rules` (default true, `--rules=false` to skip) generates or
  extends `AGENTS.md` plus per-client pointers (`CLAUDE.md`, `GEMINI.md`,
  `.github/copilot-instructions.md`); `--skill` installs the
  `documentum-investigate` skill under `.claude/skills/` and
  `.agents/skills/`; `--hooks` installs a background-resync post-commit git
  hook (opt-in); `--in-repo` puts the index under `.documentum/` instead of
  the user cache. Flow: indexing → MCP registration → rules-file generation →
  optional skill and git hook.
- **`changes [PATHS...] [--base REF] [--head REF] [--depth 2] [--json]`**:
  impact of a diff: symbols touched, their transitive callers (depth clamped
  to 1-5), and the document passages that mention them (see §5). The diff is
  `git diff -U0 BASE HEAD -- PATHS` if `--base` is given (`--head` defaults to
  `HEAD`); otherwise a diff piped on stdin, filtered by `PATHS`; otherwise
  `HEAD~1` against `--head`/`HEAD`. Text output lists touched symbols, callers,
  documents, and files unknown to the graph; `--json` prints the full report.
  Requires an existing index.
- **`explore SYMBOL [--depth 1] [--max-tokens 2000] [--json]`**: signature,
  doc, file/lines, body (truncated to the token budget with an explicit
  truncation notice), callers and callees (depth clamped to 1-5), and the
  document passages that mention the symbol. Several matches → a candidate
  list. Requires an existing index.
- **`context QUESTION [--scope auto|docs|code] [--max-tokens 2000] [--page 1]
  [--json]`**: same behaviour as the MCP tool `context` (§4.3), exposed on the
  CLI. Text output starts with a ledger line (page, tokens, more pages?),
  then one block per entry.
- **`stats [-w|--workspace FILTER] [--json] [--purge]`**: global or
  per-workspace metrics of MCP calls (volume, success/failure, duration,
  estimated tokens); `--purge` deletes the local telemetry database and exits.

A hidden subcommand `q` is the former name of `search` (§2.5).

### 3.1 `setup`: MCP registration per client

The server is always registered under the key **`documentum`**. For a **user**
client (global scope, never pinned to a repo except via `--path`), the written
arguments include `["mcp", "--path", "<absolute-repo>"]`; for a **project**
client, only `["mcp"]` (the client launches from the project directory). An
environment variable `DOCUMENTUM_CLIENT` (value = client identifier, e.g.
`"claude-code"`, `"cursor"`, `"vscode"`, `"gemini-cli"`, `"antigravity"`,
`"codex"`, `"windsurf"`, `"claude-desktop"`, `"zed"`) is added to the entry.

| Client | Scope | Config file | Root key | Entry shape |
|---|---|---|---|---|
| Claude Code | project | `.mcp.json` | `mcpServers.documentum` | `{command, args, env}` |
| Cursor | project | `.cursor/mcp.json` | `mcpServers.documentum` | same as Claude |
| VS Code / Copilot | project | `.vscode/mcp.json` | `servers.documentum` | `{type: "stdio", command, args, env}` |
| Gemini CLI | project | `.gemini/settings.json` | `mcpServers.documentum` | same as Claude |
| Antigravity (project) | project | `.agents/mcp_config.json` | `mcpServers.documentum` | same as Claude |
| Zed | project | `.zed/settings.json` | `context_servers.documentum` | `{source: "custom", command, args, env}` |
| Generic | project | `.mcp.json` | `mcpServers.documentum` | identical to Claude Code (alias) |
| Antigravity (user) | user | `~/.gemini/config/mcp_config.json` | `mcpServers.documentum` | same + `--path` |
| Codex CLI | user | `~/.codex/config.toml` | `[mcp_servers.documentum]` (TOML) | `command`, `args`, `env` as a TOML table |
| Windsurf | user | `~/.codeium/windsurf/mcp_config.json` | `mcpServers.documentum` | same + `--path` |
| Claude Desktop | user | macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`; Linux: `~/.config/Claude/claude_desktop_config.json`; Windows: `%APPDATA%\Claude\claude_desktop_config.json` | `mcpServers.documentum` | same + `--path` |

Common rules: read the existing file if present (JSON or TOML depending on the
client), **merge** while preserving any other entry already present (never
overwrite the whole file), write cleanly formatted output (*pretty-printed*
JSON, TOML via comment- and formatting-preserving editing). If the file exists
but is **not** a valid object/table at the root, or the root key is not itself
an object: explicit refusal to write, actionable error message, file
untouched. Before any modification that actually changes the content, a
timestamped **backup** is written under the workspace cache
(`<cache>/backups/<file-name>.<timestamp>.bak`); a rerun that changes nothing
touches neither the file nor the backups. `--uninstall` removes only the
`documentum` key and leaves the rest intact. "Client present" detection (for
`--client all`) by a marker specific to each project client (`.cursor/`,
`.vscode/`, `.zed/`, `.gemini/`, `.agents/` directory, or presence of the
config file itself); user clients are **never** auto-detected by
`--client all`, only named explicitly: a global config is not touched without
being asked for by name.

Rules files (pointer to `AGENTS.md`, added only if absent from the file):
`CLAUDE.md` (`@AGENTS.md`) for Claude Code, `GEMINI.md` (`@./AGENTS.md`) for
Gemini, `.github/copilot-instructions.md` for VS Code/Copilot. No dedicated
rules file for Cursor (reads `AGENTS.md` natively), Zed, Antigravity, Codex,
Windsurf, Claude Desktop.

---

## 4. MCP: protocol and tools

### 4.1 Protocol (stdio, JSON-RPC 2.0)

- **Transport**: JSON **one line = one message**, no `Content-Length` framing.
  Blocking line-by-line read on stdin, immediately flushed write on stdout.
  **stdout is reserved for the protocol**: every trace/log goes to **stderr**
  (chatty libraries run in subprocesses if needed; see PDF conversion, §2.3).
  Silent startup: no output on stdout before the first legitimate response.
- **Accepted protocol versions**: at least `2024-11-05`; ideally also the
  more recent versions known at implementation time, falling back to the most
  recent supported one if the client asks for an unknown one.
- **`initialize`** (request/response): receives the client's protocol version;
  answers with `protocolVersion` (the requested one if supported, otherwise the
  most recent known to the server), `serverInfo` (name `documentum`, version),
  `capabilities` (at least `{"tools": {"listChanged": false}}`: no dynamic tool
  list), and textual `instructions` briefly describing the available tools and
  when to use them.
- **`tools/list`** (request/response): `{"tools": [...]}`, each tool described
  by `{name, description, inputSchema}` (JSON Schema).
- **`tools/call`** (request/response): `params = {name, arguments}`; response
  `{"content": [{"type": "text", "text": "..."}], "isError": bool}` (`isError`
  omitted or `false` on success). A **tool execution** error (invalid business
  parameter, symbol not found...) is rendered as an `isError: true` result with
  a text message, **not** as a JSON-RPC error. Only protocol errors (unknown
  method, malformed JSON-RPC parameters) use the standard `error` field (codes
  `-32700` parse, `-32601` method not found, `-32602` invalid params, `-32603`
  internal).
- **Notifications** (`id` absent or `null`): handled without a response, per
  JSON-RPC 2.0 (e.g. `notifications/initialized`).
- **`ping`**: empty response `{}`.

### 4.2 Workspace selection

Every tool accepts an optional `workspace` parameter (string). Not provided or
empty → default workspace (the first declared at server startup, `mcp
--workspace`). A name matching no known workspace → explicit error listing the
available names.

### 4.3 Tools

The server exposes exactly twelve tools: six on the code graph (`code_*`) and
six on the documentation index. Token budgets use the ratio of about 4
characters per token. Whenever a budget cuts an output, the result says so
(`truncated`, `*_omitted` counters, `hint`).

**`search`**: hybrid search over the documentation (§1.6).
- Input: `query` (required), `limit` (default 5, clamped to 1-50), `tags[]`,
  `source_types[]`, `since`, `path_prefixes[]` (= `SearchFilters`, §1.5),
  `max_chars` (default 0 = no truncation; bounds the serialised size of the
  results), `workspace`.
- Output: `{query, results[]}`, each result `{rank, score, source, lines:
  [start, end], title, breadcrumb, text, meta, origin}` plus the aliases
  `path` (= `source`), `line` (= first line) and `section` (= `title`);
  `truncated: true` if `max_chars` cut the list.

**`context`**: documents and code retrieved together in one round trip, with
a token budget and pagination.
- Input: `query` (required), `scope` (`"auto"`|`"docs"`|`"code"`, default
  `"auto"`; anything else is an error), `max_tokens` (default 2000), `page`
  (default 1), `workspace`.
- Entries: unless `scope` is `code`, the 12 best document passages (search
  results with `type: "specification"`); unless `scope` is `docs`, up to 12
  code symbols matching the query or one of its identifier-like words
  (`type: "code_symbol"`: `symbol`, `file`, `line`, `kind`, `signature`,
  `doc_comment`, `snippet` (first 40 lines of the body), `callers[]`,
  `callees[]`).
- Output: `entries[]` (the current page), `ledger` (`current_page`,
  `total_pages`, `total_entries`, `has_more`, `tokens_used`, `token_budget`,
  `excluded_entities[]` listing each later page with its `count` and a
  `fetch_command`, `next_page_command`), `truncated` (true when more pages
  exist). Entries are packed greedily into pages that fit the budget: an
  entry is never cut in two, and each page holds at least one entry.

**`inventory`**, **`map`**, **`status`**, **`annotate`**: same contracts as
the homonymous CLI subcommands (§3), with the additional `workspace`
parameter. `inventory`: `kind`, `filter`, `limit` (default 50). `map`:
`prefix`, `depth` (default 2), `limit` (default 200). `annotate`: `path`
(required), `summary`, `tags[]`, `author` (default `cli`).

**`code_symbol`**: resolves a symbol by name (exact, then terminal qualified
name, then prefix, then case-insensitive).
- Input: `name` (required), `max_tokens` (default 2000), `start_line` /
  `end_line` (optional body window), `workspace`.
- Output: `symbol` (qualified name), `name`, `kind`, `lang`, `file`,
  `start_line`, `end_line`, `signature`, `doc_comment`, `body_snippet`, and,
  if the body was truncated, `body_lines_omitted` and `hint`.
- No match → tool error. Several matches → a non-error result `{ambiguous:
  true, query, candidates[]}` (each `symbol`, `name`, `kind`, `file`, `line`,
  `reason`).

**`code_callers`** / **`code_callees`**: callers (resp. callees) of a symbol,
breadth-first, never revisiting a node.
- Input: `name` (required), `depth` (1-3, default 1), `max_tokens` (default
  2000), `workspace`.
- Output: `symbol`, `direction` (`callers`|`callees`), `depth`, `edges[]`,
  `ambiguous_calls[]`; if cut by the budget, `truncated`, `edges_omitted`,
  `hint`. Same ambiguity behaviour as `code_symbol`.

**`code_path`**: shortest call path between two symbols (forward edges only).
- Input: `from`, `to` (required), `max_depth` (default 5, clamped to 1-10),
  `max_tokens` (default 2000), `workspace`.
- Output: `{reachable: false, from, to, max_depth}`, or `{reachable: true,
  from, to, distance, steps[]}` (with `truncated`, `steps_omitted` if cut).

**`code_impact`**: impact of a diff.
- Input: `diff` (a `git diff -U0` text), or `base` (default `HEAD~1`) and
  `head` (default `HEAD`) from which the tool runs `git diff -U0` itself;
  `depth` (1-5, default 3), `max_tokens` (default 3000), `workspace`.
- Output: `depth`, `touched[]` (symbols touched by the changed line ranges),
  `transitive_callers[]`, `unresolved_files[]` (paths the graph does not
  know); if cut, `truncated`, `touched_omitted`, `callers_omitted`.

**`code_sync`**: reindexes the code graph.
- Input: `paths[]` (relative paths; omit for a full resync of the tree),
  `workspace`.
- Output: `{mode: "full", report}`, or for a targeted run `{mode: "partial",
  requested, extracted, removed, skipped_unsupported_extension, errors[]}`
  (created or modified files are re-extracted, missing ones are removed).

### 4.4 Out of scope

The server has no tool for spec↔code conformance checks, architecture
mapping, raw HTTP routes, infrastructure manifests, telemetry or raw analysis
problems, and the code graph does not treat OpenAPI, AsyncAPI, GraphQL,
Prisma, BPMN, DMN, Gherkin, Terraform or APIM files as first-class formats.
Analysis problems are only used internally to feed `status` and `doctor`.
MCP clients are all handled alike (§3.1): no protocol-level special case for
any of them.

---

## 5. Code graph: external interface only

The extraction algorithm is specified in `docs/v3/CODEGRAPH.md` (generic lexer
+ scope tree + per-language recognizers, `src/code/model.rs` contract: `Lang`,
`SymbolKind`, `Symbol`, `Call`, `Import`, `FileOutline`, `extract(lang, source)
-> FileOutline`, `EXTRACTOR_VERSION`). This document only recalls the
**questions** the graph answers, for consistency with sections 1-4:

- Resolve a **symbol by name** (exact, then terminal qualified name, then
  prefix, then case-insensitive) → its signature, its doc, its file/lines, its
  source body (`code_symbol`, `explore`).
- List the **callers** and **callees** of a symbol, to a bounded depth, without
  revisiting a node (breadth-first walk) (`code_callers`, `code_callees`).
- Find the **shortest call path** between two symbols, with a bounded maximum
  depth (`code_path`).
- Assess the **impact of a git diff** (`git diff -U0`): touched files/lines →
  affected symbols → their transitive callers → (CLI `changes` only) the
  documents that mention them (`code_impact`, `changes`).
- Find the **doc↔code links**: documentation passages whose `symbol_references`
  (§1.3) match the name or qualified name of a symbol.

See `docs/v3/CODEGRAPH.md` for the storage detail (`code_files`,
`code_symbols`, `code_calls`, `code_imports`), the `callee → callee_id`
resolution, and the robustness requirements.

---

## 6. Distribution: one-line install

- **`install.sh`** (POSIX sh): OS/architecture detection (Linux and macOS,
  x86_64 and aarch64; anything else is refused), download of the asset of the
  latest GitHub release (or of a tag pinned via `DOCUMENTUM_VERSION`),
  **mandatory SHA256 verification** against the `SHA256SUMS` published in the
  release (refuses to install an unverified binary if `SHA256SUMS` is missing
  or does not cover the asset), installation into `~/.local/bin` by default
  (`DOCUMENTUM_BINDIR` to override), without root. `GH_TOKEN`/`GITHUB_TOKEN` is
  only needed for private forks or to lift API rate limits. Fallback if the
  download fails and `cargo` is available: `cargo install --git` from the
  source repository.
- **`scripts/release.sh vX.Y.Z`**: checks that the working tree is clean, that
  `Cargo.toml` already carries version `X.Y.Z` (otherwise refuses and asks to
  bump `Cargo.toml`/`Cargo.lock` first), creates an annotated tag and pushes
  it; everything else (build, publication) is delegated to the GitHub action
  triggered by the tag.
- **`.github/workflows/ci.yml`**: `cargo test --release --locked` on push to
  `master` and on every pull request.
- **`.github/workflows/release.yml`**: triggered on a `v*` tag (or manually
  with an existing tag). Matrix: Linux x86_64/aarch64 (`cargo zigbuild`, static
  musl target, no glibc dependency) and macOS aarch64/x86_64 (built on an
  Apple silicon runner). The build injects `DOCUMENTUM_VERSION`; `build.rs`
  exposes the target triple as `DOCUMENTUM_TARGET`, both shown by
  `documentum --version`. Packaging: `.tar.gz`. Publication: `sha256sum` over
  all artifacts → `SHA256SUMS`, upload to the GitHub release (created if
  absent, `--clobber` otherwise, `--generate-notes --latest`).

---

## 7. Acceptance criteria

### 7.1 Relevance gate (never ship a silent regression)

Reference: `docs/ROADMAP.md`, and the gate stated in the repository's
`CLAUDE.md`: **`bench/eval_docs.py`**. Any change touching chunking,
tokenisation, fusion, embeddings or stores must replay this gate and report
the MRR before/after, not a "looks better" judgement.

- Dataset: a private golden set (`eval/golden.jsonl`, ignored by git, or the
  file named by `$DOCUMENTUM_GOLDEN`; format in `eval/README.md`): one JSON
  line per question, `{"q": "...", "expect": [substrings, one is enough]}` or
  `{"q": "...", "all": [substrings, all must co-occur in the same chunk:
  source + breadcrumb + text]}`, `"kind"`: `exact` or `semantic`, `"split"`:
  `dev` or `test`. The reference set holds 100 questions (60 identifiers / 40
  semantic). A result "matches" at rank k if one (or all, depending on
  `expect`/`all`) of the expected substrings appears, case-insensitively, in
  the concatenated `source + breadcrumb + text` of one of the top k results.
- Metrics: MRR (mean reciprocal rank of the first relevant result) and
  recall@1/@5, with a 90% bootstrap confidence interval.
- **Threshold to hold: MRR ≥ 0.793** on the frozen reference corpus (golden set
  against the index, embedding backend `ollama`/`qwen3-embedding:0.6b`; the
  `local` backend alone does **not** hold this threshold, about 0.51 measured:
  the score fuser assumes a real semantic model on the other side). No rank
  divergence is expected on these 100 questions for a change that leaves
  chunker, BM25 and fusion untouched (§1.2, §1.6).
- Usage: `bench/eval_docs.py [--engines next] [--corpus /tmp/...] [-v]`
  replays the question set on the frozen corpus; `-v` details each failing
  question.

### 7.2 Tests (`tests/*.rs`)

- `tests/documents_test.rs`: office documents searchable, front matter drives
  the filters, every file accounted for (indexed/empty/quarantined/unusable).
- `tests/secrets_test.rs`: no secret ever reaches the index (trap set
  deliberately).
- `tests/ingest_test.rs`: intake mechanics: `ingest` (inventory + conversion +
  repo pointers, secrets quarantined, idempotence), `export-map`
  (INDEX/AGENTS/.gitignore from the front matter, refusal as long as
  `staging/` is not empty).
- `tests/daemon_test.rs`: `search` starts the daemon on demand and answers
  through it, the HTTP API serves peers, `daemon --stop` stops it (local
  embedder, no network).
- `tests/end_to_end.rs`: end-to-end CLI and MCP flows.
- Unit tests next to the code cover the rest (chunking, lexical scoring,
  settings, setup merges, MCP tools and protocol).
