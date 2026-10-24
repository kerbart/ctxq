//! MCP client-config `setup` (§3.1): idempotent registration of the `ctxq` MCP server
//! into each supported client's config file (JSON or, for Codex, TOML), with a backup before
//! any content-changing write, plus a marker-delimited `AGENTS.md`/pointer-files block.
//!
//! Everything here is pure/injectable: base directories, backup directory and timestamp are
//! always parameters, never resolved internally (no `dirs`/cache-path lookups, no clock
//! reads). The actual `setup` CLI subcommand — flag parsing, cache-dir resolution, deciding
//! *which* clients to touch for `--agent all`/`--uninstall` — is out of scope for this module
//! and is left to the team wiring the CLI.

pub mod apply;
pub mod backup;
pub mod clients;
pub mod merge_json;
pub mod merge_toml;
pub mod rules;

pub use apply::{apply_client, Outcome};
pub use clients::{
    client_by_id, detect_project_clients, detect_setup_clients, ClientSpec, ConfigFormat,
    EntryShape, Scope, CLIENTS,
};
