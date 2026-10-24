# ctxq v3: the in-house code graph

Designed from scratch for v3, with no third-party parser.

## Principle

We do not try to build a complete AST. For a navigation graph, three things
are enough:

1. **A generic lexer**, tuned per language by a table (`LangSpec`). It knows
   comments (line, block, nested blocks), strings (including raw strings,
   template literals, triple quotes, C# verbatim strings), characters, numbers
   and identifiers. It emits `Token`s with line and column. Doc comments stay
   as tokens (they are attached to the symbol that follows).
2. **A scope tree** (`Scope`): `{}` / `()` / `[]` are paired in brace languages,
   and indentation is followed for Python. A declaration opens a named scope,
   so we know the end line, the parent (the qualified name) and the body.
3. **Declaration recognizers**, one per language. They work on the tokens at
   the start of a statement within a given scope, not on lines: Allman style,
   stacked modifiers, attributes, generics and multi-line parameters are
   therefore handled out of the box.

Calls are spotted in the body: an identifier (possibly `a.b.c` or `a::b`)
followed by `(` (or `!(` for Rust macros), excluding language keywords.
Imports are collected to resolve calls.

## Languages (v3.0)

Rust, TypeScript/JavaScript (+ TSX/JSX), Python, Go, Java, Kotlin, C#.
To add a language: a `LangSpec` and a recognizer. Everything else (scopes,
calls, storage, queries) is shared.

## Contract (`src/code/model.rs`)

```text
Lang        = Rust | TypeScript | JavaScript | Python | Go | Java | Kotlin | CSharp
SymbolKind  = Function | Method | Constructor | Class | Struct | Enum | Interface
            | Trait | TypeAlias | Const | Variable | Module | Field
Symbol      { name, qname (Outer.Inner.member, separator "."), kind, lang,
              start_line, end_line (1-based, inclusive), signature (the head of
              the declaration up to the body, excluded, whitespace normalised, ≤ 300 chars),
              doc (attached comments/docstring, without markers), parent_qname }
Call        { caller_qname, callee (text as written: "foo", "self.bar",
              "Type::new", "obj.method"), line }
Import      { path (imported module/file as written), names (empty = everything) }
FileOutline { lang, symbols, calls, imports }
extract(lang, source: &str) -> FileOutline      // pure, no I/O, never panics
lang_for_path(path) -> Option<Lang>
EXTRACTOR_VERSION: u32  // bump on every extraction change → full re-extraction
```

Requirements: `extract` is **total** (an invalid file yields a partial result,
never a panic or an infinite loop) and linear in input size. A 1 MB file is
extracted in under 50 ms in release.

## Storage (`src/code/store.rs`, SQLite, same database as the docs)

```text
code_files   (path PK, lang, mtime, size, sha256)
code_symbols (id PK, path, name, qname, kind, start_line, end_line, signature, doc)
code_calls   (caller_id → code_symbols.id, callee TEXT, callee_id NULL|id, line)
code_imports (path, target, names)
```

`callee → callee_id` resolution, after each sync, in order of preference: same
file → files imported by the caller → same directory → name unique across the
whole repository. For `a.b()`, the qualified name `A.b` is tried first (the
type, if known), then the terminal name `b`. If several candidates remain,
`callee_id` stays NULL and the query shows them all as "ambiguous".

## Queries (`src/code/query.rs`)

- `find(name)`: exact name, then terminal qualified name, then prefix, then
  case-insensitive. Returns the `Symbol`s with their path.
- `body(symbol)`: rereads the file over `start_line..=end_line`.
- `callers(symbol, depth)` / `callees(symbol, depth)`: breadth-first walk,
  never revisiting a node.
- `path(a, b, max_depth)`: shortest call path from `a` to `b`.
- `impact(diff)`: takes a `git diff -U0` (text), extracts the `(path, lines)`
  hunks, finds the touched symbols, then their transitive callers (default
  depth 3) and the docs that mention them.
- `doc_links(symbol)`: doc passages whose `symbol_references` match `name` or
  `qname`.

## Tests

The fixtures are written by us, in `tests/fixtures/code/<lang>/`, with one
`expected.json` per file: expected symbols (qname, kind, start, end) and calls.
`cargo test code::` must pass on all of them. Mandatory cases: C# Allman,
stacked modifiers, attributes/decorators, nested generics `<A<B>>`, multi-line
parameters, strings containing `{`/`}`/`//`, nested Rust comments, TS template
literals with `${}`, Python f-strings and docstrings, assigned arrow functions,
Go receiver methods, Rust `impl Trait for Type`, Java/Kotlin inner classes,
`=>` expression-bodied members, records.
