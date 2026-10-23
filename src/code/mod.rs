//! Home-grown code graph: lexer → scopes → per-language declaration recognizers.
//! See docs/v3/CODEGRAPH.md.

pub mod extract;
pub mod lang;
pub mod lex;
pub mod model;
pub mod query;
pub mod scope;
pub mod store;
pub mod sync;

#[cfg(test)]
mod tests;

pub use extract::extract;
pub use model::*;
