//! Core indexing, search, parsing, and document engines of `ctxq`.

pub mod agent;
pub mod collect;
pub mod convert;
pub mod docmeta;
pub mod doctext;
pub mod embed_hint;
pub mod embed_http;
pub mod hybrid;
pub mod ingest;
pub mod lexical;
pub mod mdchunk;
pub mod parse_issues;
pub mod parser;
pub mod ranking;
pub mod secrets;
pub mod settings;
pub mod sources;
pub mod sqlite;
pub mod store;
pub mod vector;
