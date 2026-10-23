//! Per-client catalog for `setup` (§3.1): which config file, in what format, under which
//! JSON-pointer-ish key path, with what entry shape. Pure data + pure helpers — writing the
//! actual CLI subcommand (flag parsing, cache-dir resolution) is out of scope here.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Pinned to a repo only incidentally (the client is launched from inside it); the
    /// written args are just `["mcp"]`.
    Project,
    /// Global/user-level config: never pinned to a repo unless explicitly named, and always
    /// gets `["mcp", "--path", "<repo-absolu>"]`.
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    /// `{"command", "args", "env"}` — Claude/Cursor/Gemini/Antigravity/Generic/Windsurf/
    /// Claude Desktop/Codex (TOML table, same fields).
    Standard,
    /// VS Code: `{"type": "stdio", "command", "args", "env"}`.
    VsCodeStdio,
    /// Zed: `{"source": "custom", "command", "args", "env"}`.
    ZedCustom,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientSpec {
    /// Stable identifier, matches the `--agent` values where the spec names one directly
    /// (`"antigravity-user"` is the one exception: both the project and user-scope
    /// Antigravity entries share the `--agent antigravity` name in the CLI; which one(s) a
    /// given invocation targets is a `setup` command wiring decision, not this module's).
    pub id: &'static str,
    /// Value written into the `DOCUMENTUM_CLIENT` env var of the entry (spec §3.1).
    pub env_client_id: &'static str,
    pub scope: Scope,
    pub format: ConfigFormat,
    /// Relative to the injected base directory (workspace root for `Project`, home/user
    /// config directory for `User`). Empty for `claude-desktop`, whose path is OS-dependent
    /// (see [`claude_desktop_relative_path`]).
    pub relative_config_path: &'static str,
    /// Dotted path to the table holding per-server entries, e.g. `&["mcpServers"]`,
    /// `&["servers"]`, `&["context_servers"]`, `&["mcp_servers"]` (TOML). The final
    /// `"documentum"` key is appended by the merge logic, not included here.
    pub root_key_path: &'static [&'static str],
    pub entry_shape: EntryShape,
    /// Directory (relative to the workspace root) whose presence marks this *project* client
    /// as installed, for `--agent all` auto-detection. `None` for clients detected only by
    /// their own config file already existing, and for every `User`-scope client (never
    /// auto-detected, per spec).
    pub detect_marker: Option<&'static str>,
}

/// Every client from the §3.1 table. `"generic"` is deliberately excluded from
/// auto-detection ([`detect_project_clients`]): it is the exact same target as `"claude"`
/// (`.mcp.json` / `mcpServers`), kept only so it can be named explicitly as a project-scope
/// fallback for clients without a dedicated row.
pub const CLIENTS: &[ClientSpec] = &[
    ClientSpec {
        id: "claude",
        env_client_id: "claude-code",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "cursor",
        env_client_id: "cursor",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".cursor/mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".cursor"),
    },
    ClientSpec {
        id: "vscode",
        env_client_id: "vscode",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".vscode/mcp.json",
        root_key_path: &["servers"],
        entry_shape: EntryShape::VsCodeStdio,
        detect_marker: Some(".vscode"),
    },
    ClientSpec {
        id: "gemini",
        env_client_id: "gemini-cli",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".gemini/settings.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".gemini"),
    },
    ClientSpec {
        id: "antigravity",
        env_client_id: "antigravity",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".agents/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".agents"),
    },
    ClientSpec {
        id: "zed",
        env_client_id: "zed",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".zed/settings.json",
        root_key_path: &["context_servers"],
        entry_shape: EntryShape::ZedCustom,
        detect_marker: Some(".zed"),
    },
    ClientSpec {
        id: "generic",
        env_client_id: "generic",
        scope: Scope::Project,
        format: ConfigFormat::Json,
        relative_config_path: ".mcp.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "antigravity-user",
        env_client_id: "antigravity",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: ".gemini/config/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "codex",
        env_client_id: "codex",
        scope: Scope::User,
        format: ConfigFormat::Toml,
        relative_config_path: ".codex/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "windsurf",
        env_client_id: "windsurf",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: ".codeium/windsurf/mcp_config.json",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
    ClientSpec {
        id: "claude-desktop",
        env_client_id: "claude-desktop",
        scope: Scope::User,
        format: ConfigFormat::Json,
        relative_config_path: "",
        root_key_path: &["mcpServers"],
        entry_shape: EntryShape::Standard,
        detect_marker: None,
    },
];

pub fn client_by_id(id: &str) -> Option<&'static ClientSpec> {
    CLIENTS.iter().find(|c| c.id == id)
}

/// OS-dependent suffix (relative to the user's home directory) for Claude Desktop's config
/// file. Takes `os` explicitly (rather than reading `std::env::consts::OS` itself) so every
/// branch is directly testable regardless of the host running the tests.
pub fn claude_desktop_relative_path(os: &str) -> &'static str {
    match os {
        "macos" => "Library/Application Support/Claude/claude_desktop_config.json",
        "windows" => "AppData/Roaming/Claude/claude_desktop_config.json",
        _ => ".config/Claude/claude_desktop_config.json",
    }
}

/// Resolves `spec`'s config file path under `base` (workspace root for `Project`, home
/// directory for `User`).
pub fn config_path(spec: &ClientSpec, base: &Path) -> PathBuf {
    if spec.id == "claude-desktop" {
        base.join(claude_desktop_relative_path(std::env::consts::OS))
    } else {
        base.join(spec.relative_config_path)
    }
}

/// Builds the JSON entry for `spec`: `["mcp"]` for a project client, `["mcp", "--path",
/// repo_root]` for a user client (when `repo_root` is given — always expected for `User`
/// scope, per §3.1).
pub fn build_entry(spec: &ClientSpec, command: &str, repo_root: Option<&str>) -> Value {
    let mut args = vec![Value::String("mcp".to_string())];
    if spec.scope == Scope::User {
        if let Some(root) = repo_root {
            args.push(Value::String("--path".to_string()));
            args.push(Value::String(root.to_string()));
        }
    }
    let env = json!({ "DOCUMENTUM_CLIENT": spec.env_client_id });
    match spec.entry_shape {
        EntryShape::Standard => json!({ "command": command, "args": args, "env": env }),
        EntryShape::VsCodeStdio => json!({
            "type": "stdio", "command": command, "args": args, "env": env
        }),
        EntryShape::ZedCustom => json!({
            "source": "custom", "command": command, "args": args, "env": env
        }),
    }
}

/// Project-scope clients whose dedicated marker directory (or, lacking one, own config file)
/// is present under `workspace_root` — what `--agent all` installs into. User-scope clients
/// and `"generic"` (a pure alias of `"claude"`, see [`CLIENTS`]) are never returned.
pub fn detect_project_clients(workspace_root: &Path) -> Vec<&'static ClientSpec> {
    CLIENTS
        .iter()
        .filter(|c| c.scope == Scope::Project && c.id != "generic")
        .filter(|c| {
            let marker_present = c
                .detect_marker
                .map(|m| workspace_root.join(m).is_dir())
                .unwrap_or(false);
            marker_present || config_path(c, workspace_root).is_file()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_is_an_alias_of_claude_same_file_same_keys() {
        let claude = client_by_id("claude").unwrap();
        let generic = client_by_id("generic").unwrap();
        assert_eq!(claude.relative_config_path, generic.relative_config_path);
        assert_eq!(claude.root_key_path, generic.root_key_path);
    }

    #[test]
    fn user_scope_clients_are_never_auto_detectable() {
        for spec in CLIENTS.iter().filter(|c| c.scope == Scope::User) {
            assert!(
                spec.detect_marker.is_none(),
                "{} must not be auto-detected",
                spec.id
            );
        }
    }

    #[test]
    fn claude_desktop_path_differs_per_os() {
        assert!(claude_desktop_relative_path("macos").contains("Library"));
        assert!(claude_desktop_relative_path("windows").contains("AppData"));
        assert!(claude_desktop_relative_path("linux").starts_with(".config"));
    }

    #[test]
    fn project_entry_has_no_path_argument() {
        let spec = client_by_id("cursor").unwrap();
        let entry = build_entry(spec, "documentum", Some("/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["env"]["DOCUMENTUM_CLIENT"], "cursor");
    }

    #[test]
    fn user_entry_pins_the_repo_path() {
        let spec = client_by_id("windsurf").unwrap();
        let entry = build_entry(spec, "documentum", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp", "--path", "/abs/repo"]));
    }

    #[test]
    fn vscode_entry_has_the_stdio_type_field() {
        let spec = client_by_id("vscode").unwrap();
        let entry = build_entry(spec, "documentum", None);
        assert_eq!(entry["type"], "stdio");
    }

    #[test]
    fn zed_entry_has_the_custom_source_field() {
        let spec = client_by_id("zed").unwrap();
        let entry = build_entry(spec, "documentum", None);
        assert_eq!(entry["source"], "custom");
    }

    #[test]
    fn detect_project_clients_finds_only_markers_present_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let found = detect_project_clients(dir.path());
        let ids: Vec<&str> = found.iter().map(|c| c.id).collect();
        assert!(ids.contains(&"cursor"));
        assert!(!ids.contains(&"zed"));
        assert!(!ids.contains(&"generic"));
    }

    #[test]
    fn detect_project_clients_finds_claude_by_its_own_config_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
        let ids: Vec<&str> = detect_project_clients(dir.path())
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"claude"));
    }
}
