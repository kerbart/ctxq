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
    /// Value written into the `CTXQ_CLIENT` env var of the entry (spec §3.1).
    pub env_client_id: &'static str,
    pub scope: Scope,
    pub format: ConfigFormat,
    /// Relative to the injected base directory (workspace root for `Project`, home/user
    /// config directory for `User`). Empty for `claude-desktop`, whose path is OS-dependent
    /// (see [`claude_desktop_relative_path`]).
    pub relative_config_path: &'static str,
    /// Dotted path to the table holding per-server entries, e.g. `&["mcpServers"]`,
    /// `&["servers"]`, `&["context_servers"]`, `&["mcp_servers"]` (TOML). The final
    /// `"ctxq"` key is appended by the merge logic, not included here.
    pub root_key_path: &'static [&'static str],
    pub entry_shape: EntryShape,
    /// Directory (relative to the workspace root) whose presence marks this *project* client
    /// as installed, for `--agent all` auto-detection. `None` for clients detected only by
    /// their own config file already existing, and for every `User`-scope client (never
    /// auto-detected via a project marker). Codex is the exception: see
    /// [`detect_setup_clients`].
    pub detect_marker: Option<&'static str>,
}

impl ClientSpec {
    /// User-scope clients pin `["mcp", "--path", repo]` so a global config still
    /// indexes the repo that ran `setup`. Codex does not: a pinned path would
    /// steal every other project the next time `setup` runs, and Codex already
    /// launches MCP from the session cwd (plus a project `.codex/config.toml`).
    pub fn pins_repo_path(&self) -> bool {
        self.scope == Scope::User && self.id != "codex-user"
    }

    /// Codex project entries set `cwd` so the stdio server starts in the repo
    /// even if the client process cwd is elsewhere. User-scope Codex must not:
    /// that would reintroduce last-project-wins.
    pub fn includes_cwd(&self) -> bool {
        self.id == "codex" && self.scope == Scope::Project
    }
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
        scope: Scope::Project,
        format: ConfigFormat::Toml,
        relative_config_path: ".codex/config.toml",
        root_key_path: &["mcp_servers"],
        entry_shape: EntryShape::Standard,
        detect_marker: Some(".codex"),
    },
    ClientSpec {
        id: "codex-user",
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
/// repo_root]` for a user client that [`ClientSpec::pins_repo_path`] (when `repo_root`
/// is given). Codex project entries also set `cwd`.
pub fn build_entry(spec: &ClientSpec, command: &str, repo_root: Option<&str>) -> Value {
    let mut args = vec![Value::String("mcp".to_string())];
    if spec.pins_repo_path() {
        if let Some(root) = repo_root {
            args.push(Value::String("--path".to_string()));
            args.push(Value::String(root.to_string()));
        }
    }
    let env = json!({ "CTXQ_CLIENT": spec.env_client_id });
    let mut entry = match spec.entry_shape {
        EntryShape::Standard => json!({ "command": command, "args": args, "env": env }),
        EntryShape::VsCodeStdio => json!({
            "type": "stdio", "command": command, "args": args, "env": env
        }),
        EntryShape::ZedCustom => json!({
            "source": "custom", "command": command, "args": args, "env": env
        }),
    };
    if spec.includes_cwd() {
        if let Some(root) = repo_root {
            entry["cwd"] = json!(root);
        }
    }
    entry
}

/// Project-scope clients whose dedicated marker directory (or, lacking one, own config file)
/// is present under `workspace_root`. User-scope clients and `"generic"` (a pure alias of
/// `"claude"`, see [`CLIENTS`]) are never returned. Prefer [`detect_setup_clients`] for
/// `--client all`: that also picks up Codex when it is installed in `$HOME`.
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

/// Clients `setup --client all` should register: project clients present in the
/// workspace, plus Codex (project `.codex/config.toml` *and* user
/// `~/.codex/config.toml`) when `$HOME/.codex` exists.
///
/// Codex cannot run `ctxq` in its shell sandbox (`bwrap: loopback: Failed
/// RTM_NEWADDR`). MCP registration is the only path that works out of the box,
/// so we write it without waiting for `--client codex`.
pub fn detect_setup_clients(
    workspace_root: &Path,
    home: Option<&Path>,
) -> Vec<&'static ClientSpec> {
    let mut out = detect_project_clients(workspace_root);
    let codex_installed = home.map(|h| h.join(".codex").is_dir()).unwrap_or(false);
    if codex_installed {
        for id in ["codex", "codex-user"] {
            if let Some(spec) = client_by_id(id) {
                if !out.iter().any(|s| s.id == spec.id) {
                    out.push(spec);
                }
            }
        }
    }
    out
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
        let entry = build_entry(spec, "ctxq", Some("/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["env"]["CTXQ_CLIENT"], "cursor");
    }

    #[test]
    fn user_entry_pins_the_repo_path() {
        let spec = client_by_id("windsurf").unwrap();
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp", "--path", "/abs/repo"]));
    }

    #[test]
    fn vscode_entry_has_the_stdio_type_field() {
        let spec = client_by_id("vscode").unwrap();
        let entry = build_entry(spec, "ctxq", None);
        assert_eq!(entry["type"], "stdio");
    }

    #[test]
    fn zed_entry_has_the_custom_source_field() {
        let spec = client_by_id("zed").unwrap();
        let entry = build_entry(spec, "ctxq", None);
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

    #[test]
    fn detect_project_clients_finds_codex_by_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let ids: Vec<&str> = detect_project_clients(dir.path())
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"codex"));
        assert!(!ids.contains(&"codex-user"));
    }

    #[test]
    fn codex_project_entry_sets_cwd_and_does_not_pin_path() {
        let spec = client_by_id("codex").unwrap();
        assert_eq!(spec.scope, Scope::Project);
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["cwd"], "/abs/repo");
        assert_eq!(entry["env"]["CTXQ_CLIENT"], "codex");
    }

    #[test]
    fn codex_user_entry_does_not_pin_the_repo_path() {
        let spec = client_by_id("codex-user").unwrap();
        assert_eq!(spec.scope, Scope::User);
        let entry = build_entry(spec, "ctxq", Some("/abs/repo"));
        assert_eq!(entry["args"], json!(["mcp"]));
        assert!(entry.get("cwd").is_none());
    }

    #[test]
    fn detect_setup_clients_includes_codex_when_home_has_dot_codex() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ids: Vec<&str> = detect_setup_clients(workspace.path(), Some(home.path()))
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"codex-user"));
    }

    #[test]
    fn detect_setup_clients_skips_codex_when_not_installed() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ids: Vec<&str> = detect_setup_clients(workspace.path(), Some(home.path()))
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(!ids.contains(&"codex"));
        assert!(!ids.contains(&"codex-user"));
    }
}
