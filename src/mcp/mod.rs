//! MCP stdio server (JSON-RPC 2.0, one JSON object per line). See `docs/v3/SPEC.md` §4.
//! Tool names and protocol plumbing are local to this crate. `DocsBackend` is implemented
//! on `DocEngine` in [`docs_engine`].

pub mod args;
pub mod budget;
pub mod docs_engine;
pub mod jsonrpc;
pub mod server;
pub mod tools;

pub use server::{McpServer, WorkspaceConfig};
pub use tools::DocsBackend;
