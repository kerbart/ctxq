//! Data model shared by the code extractor, the store and the queries.
//! See docs/v3/CODEGRAPH.md.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Bump whenever extraction output changes: indexes built by an older binary are re-extracted.
pub const EXTRACTOR_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    Kotlin,
    CSharp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Constructor,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    TypeAlias,
    Const,
    Variable,
    Module,
    Field,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// Dotted path from the outermost declaration: `Outer.Inner.member`.
    pub qname: String,
    pub kind: SymbolKind,
    pub lang: Lang,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    /// Declaration head up to (excluding) the body, whitespace-normalised, ≤ 300 chars.
    pub signature: String,
    /// Attached doc comments / docstring, markers stripped.
    pub doc: String,
    pub parent_qname: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub caller_qname: String,
    /// As written: `foo`, `self.bar`, `Type::new`, `obj.method`.
    pub callee: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub path: String,
    /// Imported names; empty = whole module.
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileOutline {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
}

impl Lang {
    pub fn for_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Lang::Rust,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            "cs" => Lang::CSharp,
            _ => return None,
        })
    }
}

/// Infers a supported language from a source path.
pub fn lang_for_path(path: &Path) -> Option<Lang> {
    Lang::for_path(path)
}
