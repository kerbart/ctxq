//! # ctxq
//!
//! Indexing and retrieval for agents, in one binary:
//! - [`core`]: documents (markdown, office, PDF, inventories) with hybrid search
//!   (BM25 + embeddings), the daemon, ingest and stores;
//! - [`code`]: a home-grown code graph (lexer → scopes → per-language
//!   recognizers), its SQLite store and queries (symbols, callers, impact);
//! - [`mcp`]: the stdio MCP server exposing both;
//! - [`setup`]: MCP registration for the supported clients.

pub mod cli;
pub mod code;
pub mod core;
pub mod mcp;
pub mod setup;

/// Binary, crate and MCP server name — one identifier everywhere.
pub const NAME: &str = "ctxq";

/// Release tag injected by CI, else the crate version.
pub const VERSION: &str = match option_env!("CTXQ_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Target triple injected by CI.
pub const TARGET: &str = match option_env!("CTXQ_TARGET") {
    Some(t) => t,
    None => "unknown-target",
};

/// `ctxq --version` long form.
pub fn long_version() -> &'static str {
    static LONG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
        "{VERSION} ({TARGET})\nsqlite {}\ncode extractor v{}\nembeddings: local, ollama, openai{}",
        rusqlite::version(),
        code::EXTRACTOR_VERSION,
        if cfg!(feature = "onnx-embeddings") { ", onnx" } else { "" },
    )
    });
    &LONG
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_version_names_the_engines() {
        let v = super::long_version();
        assert!(v.contains("sqlite") && v.contains("code extractor"));
    }
}
